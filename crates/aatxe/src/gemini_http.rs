//! Gemini proxy — Google's Gemini API as an [`LlmClient`] backend, over its
//! OpenAI-compatible chat-completions surface.
//!
//! ## Why a third backend
//!
//! `pi-proxy` and `claude-code` both shell out to a local coding-agent CLI
//! and can `read`/`grep`/`glob` the repo under review. Gemini has no such
//! agent binary, so this backend is a *direct* blocking HTTP client over
//! [`ureq`] — the same dependency the sticky-comment poster
//! ([`crate::github_http`]) already uses. Gemini sees exactly the
//! pre-packed prompt the pipeline builds (diff + AST scope + related-file
//! context) and nothing else; it has no repo tool access. That makes it
//! the "pre-packed context, no tools" arm of the backend experiment — and
//! the cheapest-to-operate option (one API key, no local CLI install).
//!
//! ## Wire shape
//!
//! Gemini exposes an OpenAI-compatible endpoint:
//!
//! ```text
//! POST https://generativelanguage.googleapis.com/v1beta/openai/chat/completions
//! Authorization: Bearer $GEMINI_API_KEY
//! { "model", "messages":[{role,content}], "temperature", "max_tokens",
//!   "response_format": {"type":"json_object"} }
//! ```
//!
//! The response is the standard OpenAI shape
//! (`choices[].message.content` + `usage.{prompt,completion}_tokens`).
//! Because the trait was designed against OpenAI-compatible Kimi in the
//! first place ([`aatxe_council::llm`]), the mapping is one-to-one.
//!
//! ## Resilience
//!
//! Transient failures (transport errors, `408`/`425`/`429`, any `5xx`) are
//! retried with exponential backoff up to [`GeminiConfig::max_retries`].
//! Non-retriable HTTP statuses surface as [`LlmError::Status`] so the
//! council's fail-soft path captures them into `AgentReview.error` and the
//! surviving proposers keep running. Like the subprocess backends we strip
//! a wrapping markdown fence from the content (`response_format` should
//! prevent it, but models occasionally add one anyway).

use crate::subprocess_llm::sanitize_text_output;
use aatxe_council::llm::{ChatRequest, ChatResponse, LlmClient, LlmError, Role};
use serde::Deserialize;
use std::env;
use std::thread;
use std::time::Duration;

/// Default OpenAI-compatible chat-completions endpoint.
const DEFAULT_BASE_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions";
/// Default model. A strong, low-latency general model; override with
/// `GEMINI_MODEL` (e.g. `gemini-2.5-pro` for higher quality at more cost).
const DEFAULT_MODEL: &str = "gemini-2.5-flash";

/// Read `GEMINI_MODEL` or fall back to [`DEFAULT_MODEL`]. Infallible and
/// key-free so the report-header model display works even when no API key
/// is set (the key requirement is enforced at client construction).
pub fn model_from_env() -> String {
    env::var("GEMINI_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// Endpoint, credentials, model, and retry/timeout budget for the Gemini
/// backend.
#[derive(Debug, Clone)]
pub struct GeminiConfig {
    /// Bearer token. Required — there is no anonymous Gemini access.
    pub api_key: String,
    /// Model id passed in the request body. Defaults to [`DEFAULT_MODEL`],
    /// overridable via `GEMINI_MODEL` or `aatxe council --model`.
    pub model: String,
    /// Full chat-completions URL. Overridable via `GEMINI_BASE_URL` (handy
    /// for a regional endpoint or a local proxy in tests).
    pub base_url: String,
    /// Per-request wall-clock budget. Defaults to 10 minutes to match the
    /// subprocess backends' contract.
    pub timeout: Duration,
    /// Maximum number of *retries* (so total attempts = `max_retries + 1`)
    /// on transient failures. Defaults to 4.
    pub max_retries: u32,
    /// Base backoff between retries; doubles each attempt. Defaults to
    /// 500ms. Lowered to near-zero in tests.
    pub base_backoff: Duration,
}

impl GeminiConfig {
    /// Discover from the environment. Reads `GEMINI_API_KEY` (required),
    /// `GEMINI_MODEL` (optional), and `GEMINI_BASE_URL` (optional). Errors
    /// with a [`LlmError::Transport`] carrying an actionable message when
    /// the key is absent — fail fast at construction rather than letting
    /// every proposer call 401 mid-run.
    pub fn from_env() -> Result<Self, LlmError> {
        let api_key = env::var("GEMINI_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                LlmError::Transport(
                    "GEMINI_API_KEY is not set — the Gemini backend needs it to authenticate \
                     against the Generative Language API"
                        .to_string(),
                )
            })?;
        let base_url = env::var("GEMINI_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Ok(Self {
            api_key,
            model: model_from_env(),
            base_url,
            timeout: Duration::from_secs(600),
            max_retries: 4,
            base_backoff: Duration::from_millis(500),
        })
    }
}

/// Gemini HTTP client. One instance per council run; cheap to clone (the
/// `ureq::Agent` is internally reference-counted).
#[derive(Debug, Clone)]
pub struct GeminiClient {
    config: GeminiConfig,
    agent: ureq::Agent,
}

impl GeminiClient {
    pub fn new(config: GeminiConfig) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(config.timeout)
            .timeout_write(config.timeout)
            .build();
        Self { config, agent }
    }
}

impl LlmClient for GeminiClient {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let body = request_body(&self.config.model, &req);

        // Retry transient failures with exponential backoff. `attempt`
        // counts from 1; we stop after `max_retries` *additional* tries.
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let result = self
                .agent
                .post(&self.config.base_url)
                .set("Authorization", &format!("Bearer {}", self.config.api_key))
                .set("Content-Type", "application/json")
                .send_json(body.clone());

            match result {
                Ok(resp) => {
                    let parsed: OpenAiChatResponse = resp
                        .into_json()
                        .map_err(|e| LlmError::Parse(format!("decoding Gemini response: {e}")))?;
                    return parse_response(parsed);
                }
                Err(ureq::Error::Status(code, resp)) => {
                    if is_retriable_status(code) && attempt <= self.config.max_retries {
                        thread::sleep(backoff_delay(self.config.base_backoff, attempt));
                        continue;
                    }
                    // Read at most the first ~2KB of the error body for the
                    // message; ureq caps `into_string` at 10MB but the API
                    // error JSON is tiny.
                    let body = resp.into_string().unwrap_or_default();
                    return Err(LlmError::Status { status: code, body });
                }
                Err(ureq::Error::Transport(t)) => {
                    if attempt <= self.config.max_retries {
                        thread::sleep(backoff_delay(self.config.base_backoff, attempt));
                        continue;
                    }
                    return Err(LlmError::Transport(t.to_string()));
                }
            }
        }
    }
}

/// Build the OpenAI-compatible request body. Factored out (and taking the
/// model explicitly) so tests can assert the wire shape without a client
/// or a network. `response_format: json_object` is only set when the
/// pipeline asks for JSON — pure-text turns omit it.
fn request_body(model: &str, req: &ChatRequest) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": role_str(m.role),
                "content": m.content,
            })
        })
        .collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "temperature": req.temperature,
        "max_tokens": req.max_tokens,
    });
    if req.json_only {
        body["response_format"] = serde_json::json!({ "type": "json_object" });
    }
    body
}

/// Map the pipeline's `Role` to the OpenAI wire string.
fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

/// Statuses worth retrying: request timeout, too-early, rate-limit, and any
/// server-side 5xx. Everything else (4xx auth/validation) is terminal.
fn is_retriable_status(code: u16) -> bool {
    matches!(code, 408 | 425 | 429 | 500..=599)
}

/// Exponential backoff capped at 2^6 × base so a long retry chain can't
/// sleep for minutes.
fn backoff_delay(base: Duration, attempt: u32) -> Duration {
    base * 2u32.pow(attempt.saturating_sub(1).min(6))
}

/// Turn a decoded OpenAI response into a [`ChatResponse`], applying the
/// same fence-stripping the subprocess backends use. No choices, or a
/// choice whose content sanitises to empty, is an [`LlmError::Empty`] so
/// the council flags the call rather than shipping a blank review.
fn parse_response(parsed: OpenAiChatResponse) -> Result<ChatResponse, LlmError> {
    let (prompt_tokens, completion_tokens) = parsed
        .usage
        .map(|u| (u.prompt_tokens, u.completion_tokens))
        .unwrap_or((None, None));

    let choice = parsed.choices.into_iter().next().ok_or(LlmError::Empty)?;
    let finish_reason = choice.finish_reason.unwrap_or_else(|| "stop".to_string());
    let content_raw = choice.message.and_then(|m| m.content).unwrap_or_default();
    let content = sanitize_text_output(&content_raw);
    if content.is_empty() {
        return Err(LlmError::Empty);
    }

    Ok(ChatResponse {
        content,
        finish_reason,
        prompt_tokens,
        completion_tokens,
    })
}

/// Subset of the OpenAI chat-completions response we consume. Extra fields
/// (model echo, system_fingerprint, …) are ignored so an API schema growth
/// can't break the parse.
#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<RespMessage>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RespMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aatxe_council::llm::ChatMessage;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn req(json_only: bool) -> ChatRequest {
        ChatRequest {
            model: "ignored-by-client".into(),
            messages: vec![
                ChatMessage::system("PERSONA: correctness"),
                ChatMessage::user("review this diff"),
            ],
            temperature: 0.2,
            max_tokens: 256,
            json_only,
        }
    }

    #[test]
    fn request_body_maps_roles_and_params() {
        let body = request_body("gemini-2.5-flash", &req(true));
        assert_eq!(body["model"], "gemini-2.5-flash");
        // `temperature` is an f32 widened to JSON f64, so compare with a
        // tolerance rather than against the exact literal.
        assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(body["max_tokens"], 256);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "PERSONA: correctness");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "review this diff");
    }

    #[test]
    fn request_body_sets_json_response_format_only_when_asked() {
        let with_json = request_body("m", &req(true));
        assert_eq!(with_json["response_format"]["type"], "json_object");
        let without = request_body("m", &req(false));
        assert!(
            without.get("response_format").is_none(),
            "pure-text turns must omit response_format: {without}"
        );
    }

    #[test]
    fn role_str_covers_every_variant() {
        assert_eq!(role_str(Role::System), "system");
        assert_eq!(role_str(Role::User), "user");
        assert_eq!(role_str(Role::Assistant), "assistant");
    }

    #[test]
    fn retriable_status_classification() {
        for code in [408, 425, 429, 500, 502, 503, 504, 599] {
            assert!(is_retriable_status(code), "{code} should be retriable");
        }
        for code in [200, 201, 400, 401, 403, 404, 422] {
            assert!(!is_retriable_status(code), "{code} should be terminal");
        }
    }

    #[test]
    fn backoff_grows_then_caps() {
        let base = Duration::from_millis(10);
        assert_eq!(backoff_delay(base, 1), Duration::from_millis(10));
        assert_eq!(backoff_delay(base, 2), Duration::from_millis(20));
        assert_eq!(backoff_delay(base, 3), Duration::from_millis(40));
        // Capped at 2^6 × base regardless of how high the attempt climbs.
        assert_eq!(backoff_delay(base, 50), base * 64);
    }

    #[test]
    fn parse_response_extracts_content_finish_and_usage() {
        let parsed: OpenAiChatResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"{\"findings\":[]}"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":120,"completion_tokens":34}}"#,
        )
        .unwrap();
        let resp = parse_response(parsed).unwrap();
        assert_eq!(resp.content, r#"{"findings":[]}"#);
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.prompt_tokens, Some(120));
        assert_eq!(resp.completion_tokens, Some(34));
    }

    #[test]
    fn parse_response_strips_markdown_fence() {
        let parsed: OpenAiChatResponse = serde_json::from_str(
            "{\"choices\":[{\"message\":{\"content\":\"```json\\n{\\\"findings\\\":[]}\\n```\"}}]}",
        )
        .unwrap();
        let resp = parse_response(parsed).unwrap();
        assert_eq!(resp.content, r#"{"findings":[]}"#);
        // No usage block → tokens are None, not an error.
        assert!(resp.prompt_tokens.is_none());
    }

    #[test]
    fn parse_response_empty_on_no_choices() {
        let parsed: OpenAiChatResponse = serde_json::from_str(r#"{"choices":[]}"#).unwrap();
        assert!(matches!(parse_response(parsed), Err(LlmError::Empty)));
    }

    #[test]
    fn parse_response_empty_on_blank_content() {
        let parsed: OpenAiChatResponse =
            serde_json::from_str(r#"{"choices":[{"message":{"content":"   "}}]}"#).unwrap();
        assert!(matches!(parse_response(parsed), Err(LlmError::Empty)));
    }

    /// Env-var round-trip. Both the missing-key and present-key cases live
    /// in ONE test on purpose: process env is global, so splitting them into
    /// two `#[test]` fns lets cargo's parallel runner interleave a
    /// `remove_var` from one into the other and flake. Keeping every
    /// `GEMINI_*` mutation on a single thread serialises them.
    #[test]
    fn from_env_round_trips_key_model_and_base_url() {
        let saved_key = env::var("GEMINI_API_KEY").ok();
        let saved_model = env::var("GEMINI_MODEL").ok();
        let saved_base = env::var("GEMINI_BASE_URL").ok();

        // Missing key → actionable Transport error.
        env::remove_var("GEMINI_API_KEY");
        let err = GeminiConfig::from_env().unwrap_err();
        assert!(matches!(err, LlmError::Transport(msg) if msg.contains("GEMINI_API_KEY")));

        // Key + model + base all read from the environment.
        env::set_var("GEMINI_API_KEY", "test-key-123");
        env::set_var("GEMINI_MODEL", "gemini-2.5-pro");
        env::set_var("GEMINI_BASE_URL", "http://localhost:9/chat");
        let cfg = GeminiConfig::from_env().unwrap();
        assert_eq!(cfg.api_key, "test-key-123");
        assert_eq!(cfg.model, "gemini-2.5-pro");
        assert_eq!(cfg.base_url, "http://localhost:9/chat");

        // Model falls back to the compiled default when GEMINI_MODEL is absent.
        env::remove_var("GEMINI_MODEL");
        assert_eq!(GeminiConfig::from_env().unwrap().model, DEFAULT_MODEL);

        // Restore the host environment.
        match saved_key {
            Some(v) => env::set_var("GEMINI_API_KEY", v),
            None => env::remove_var("GEMINI_API_KEY"),
        }
        match saved_model {
            Some(v) => env::set_var("GEMINI_MODEL", v),
            None => env::remove_var("GEMINI_MODEL"),
        }
        match saved_base {
            Some(v) => env::set_var("GEMINI_BASE_URL", v),
            None => env::remove_var("GEMINI_BASE_URL"),
        }
    }

    // ── localhost mock-server round-trips ──────────────────────────────
    //
    // A one-shot HTTP/1.1 server bound to an ephemeral port. Each entry in
    // `responses` is served to one connection in order, then the listener
    // drops. Returns the chat URL and a handle yielding the raw request
    // text(s) the client sent (so we can assert auth header + body).

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 2048];
        loop {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            let s = String::from_utf8_lossy(&buf);
            if let Some(hdr_end) = s.find("\r\n\r\n") {
                let content_len = s[..hdr_end]
                    .lines()
                    .find_map(|l| {
                        let lower = l.to_ascii_lowercase();
                        lower
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if buf.len() >= hdr_end + 4 + content_len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn mock_server(responses: Vec<(u16, String)>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1beta/openai/chat/completions");
        let handle = thread::spawn(move || {
            let mut captured = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                captured.push(read_http_request(&mut stream));
                let reason = match status {
                    200 => "OK",
                    400 => "Bad Request",
                    429 => "Too Many Requests",
                    503 => "Service Unavailable",
                    _ => "Status",
                };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
                let _ = stream.flush();
            }
            captured
        });
        (url, handle)
    }

    fn test_config(base_url: String) -> GeminiConfig {
        GeminiConfig {
            api_key: "secret-key".into(),
            model: "gemini-2.5-flash".into(),
            base_url,
            timeout: Duration::from_secs(5),
            max_retries: 4,
            base_backoff: Duration::from_millis(1),
        }
    }

    #[test]
    fn chat_happy_path_sends_auth_and_parses_result() {
        let canned = r#"{"choices":[{"message":{"content":"{\"findings\":[]}"},
            "finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#;
        let (url, handle) = mock_server(vec![(200, canned.to_string())]);
        let client = GeminiClient::new(test_config(url));
        let resp = client.chat(req(true)).expect("chat");
        assert_eq!(resp.content, r#"{"findings":[]}"#);
        assert_eq!(resp.prompt_tokens, Some(10));
        assert_eq!(resp.completion_tokens, Some(2));

        let sent = handle.join().unwrap();
        assert_eq!(sent.len(), 1);
        let request = &sent[0];
        assert!(
            request.contains("Authorization: Bearer secret-key"),
            "auth header missing: {request}"
        );
        assert!(
            request.contains("\"model\":\"gemini-2.5-flash\""),
            "model missing from body: {request}"
        );
        assert!(
            request.contains("response_format"),
            "json_only request should carry response_format: {request}"
        );
    }

    #[test]
    fn chat_maps_terminal_status_to_status_error() {
        let (url, handle) = mock_server(vec![(
            400,
            r#"{"error":{"message":"bad request"}}"#.to_string(),
        )]);
        let client = GeminiClient::new(test_config(url));
        let err = client.chat(req(true)).unwrap_err();
        match err {
            LlmError::Status { status, body } => {
                assert_eq!(status, 400);
                assert!(body.contains("bad request"), "body={body}");
            }
            other => panic!("expected Status, got {other:?}"),
        }
        handle.join().unwrap();
    }

    #[test]
    fn chat_retries_on_503_then_succeeds() {
        let canned = r#"{"choices":[{"message":{"content":"{\"verdicts\":[]}"}}]}"#;
        let (url, handle) = mock_server(vec![
            (503, "overloaded".to_string()),
            (200, canned.to_string()),
        ]);
        let client = GeminiClient::new(test_config(url));
        let resp = client
            .chat(req(true))
            .expect("chat should succeed after retry");
        assert_eq!(resp.content, r#"{"verdicts":[]}"#);
        // Two connections served: the 503 and the retried 200.
        assert_eq!(handle.join().unwrap().len(), 2);
    }
}
