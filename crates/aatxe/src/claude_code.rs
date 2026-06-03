//! Claude Code proxy — `claude` CLI as an [`LlmClient`] backend.
//!
//! Modelled byte-for-byte on [`crate::pi_proxy`]; the only meaningful
//! differences are the spawn argv (Claude Code's `--print` surface) and
//! the output parser (Claude Code emits `--output-format json` which
//! carries the final assistant turn plus per-call usage tokens).
//!
//! ## Why a second backend
//!
//! `pi-proxy` routes through Moonshot's `kimi-coding` endpoint, which is
//! gated to coding-agent user-agents and requires `KIMI_API_KEY`.
//! `claude-code` uses the engineer's existing Claude Code
//! subscription/auth — no separate key plumbing, no UA gating, and the
//! underlying agentic loop is Anthropic's first-party tool-use stack.
//! The trait seam already exists; this is the second leg of the
//! "swap-backend" experiment captured in [[aatxe]]'s real-LLM eval log.
//!
//! ## Safety surface
//!
//! Same posture as `pi-proxy`: the council needs read-only repo access
//! to grep, glob, and read files in service of its proposers. We
//! hardcode an allowlist (`Read`, `Grep`, `Glob`) and pass
//! `--disallowed-tools` as belt-and-braces against `Bash`, `Edit`,
//! `Write`, `WebFetch`, `WebSearch`. The allowlist and denylist are
//! `const` arrays — they cannot be widened from outside this module.
//!
//! ## Output shape
//!
//! `claude --print --output-format json` produces a single JSON object:
//!
//! ```json
//! {
//!   "type": "result", "subtype": "success", "is_error": false,
//!   "result": "<final assistant message text>",
//!   "duration_ms": 1234, "total_cost_usd": 0.0123,
//!   "usage": { "input_tokens": 12, "output_tokens": 34, ... }
//! }
//! ```
//!
//! On `is_error: true` we surface as [`LlmError::Status`] so the council's
//! fail-soft path captures the error into `AgentReview.error` and the
//! surviving proposers keep running. On success we return `result`
//! (with the same fenced-JSON sanitisation as `pi-proxy`, since Claude
//! occasionally wraps JSON answers in a markdown fence even when told
//! not to) plus the usage tokens for cost telemetry.

use aatxe_council::llm::{ChatRequest, ChatResponse, LlmClient, LlmError, Role};
use serde::Deserialize;
use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Where to find the `claude` binary, which model to drive it with, and
/// how long to wait. Defaults match a freshly-installed Claude Code on
/// macOS using the subscription/keychain auth path.
#[derive(Debug, Clone)]
pub struct ClaudeCodeConfig {
    /// Path or executable name to invoke. Defaults to `"claude"` (looked
    /// up on `$PATH`).
    pub binary: PathBuf,
    /// Optional model alias passed via `--model` (e.g. `sonnet`,
    /// `opus`, or a full model id). `None` lets Claude Code pick its
    /// configured default.
    pub model: Option<String>,
    /// Optional per-call USD budget passed via `--max-budget-usd`. `None`
    /// lets Claude Code use its configured default. Pass `Some(2.0)` to
    /// cap an individual proposer call at $2.
    pub max_budget_usd: Option<f64>,
    /// Wall-clock budget for one invocation. Defaults to 10 minutes —
    /// matches `pi-proxy` so the council's wall-clock contract is the
    /// same regardless of backend.
    pub timeout: Duration,
    /// Working directory the `claude` subprocess runs in. The
    /// `Read`/`Grep`/`Glob` tools are rooted here, so this should be the
    /// repo being reviewed. Defaults to the parent process's cwd.
    pub cwd: Option<PathBuf>,
    /// Pass `--bare` to skip hooks/LSP/plugin sync/CLAUDE.md auto-discovery.
    /// Defaults to `false`: `--bare` explicitly disables OAuth + keychain
    /// auth reads (per `claude --help`: "Anthropic auth is strictly
    /// ANTHROPIC_API_KEY or apiKeyHelper via --settings"), which would
    /// break the whole "council uses your Claude Code subscription"
    /// promise on every machine that doesn't have `ANTHROPIC_API_KEY`
    /// set. Opt in with `CLAUDE_BARE=1` when you do have an API-key
    /// auth path AND want maximum determinism (no hooks, no
    /// auto-CLAUDE.md, no plugins) — for council CI use, typically yes;
    /// for an engineer running it on their laptop against their
    /// subscription, no.
    pub bare: bool,
}

impl ClaudeCodeConfig {
    /// Discover from environment. Reads four env vars: `CLAUDE_BIN`
    /// (overrides the executable path — useful when multiple Claude Code
    /// installs coexist or when the binary isn't on `$PATH`),
    /// `CLAUDE_MODEL`, `CLAUDE_MAX_BUDGET_USD`, and `CLAUDE_BARE` (set
    /// to `1` / `true` to flip the `--bare` flag on). The remaining
    /// fields (`timeout`, `cwd`) always take compiled-in defaults.
    pub fn from_env() -> Self {
        let binary = env::var("CLAUDE_BIN")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("claude"));
        let model = env::var("CLAUDE_MODEL").ok().filter(|s| !s.is_empty());
        let max_budget_usd = env::var("CLAUDE_MAX_BUDGET_USD")
            .ok()
            .and_then(|s| s.parse::<f64>().ok());
        let bare = matches!(
            env::var("CLAUDE_BARE").unwrap_or_default().as_str(),
            "1" | "true" | "TRUE" | "yes"
        );
        Self {
            binary,
            model,
            max_budget_usd,
            timeout: Duration::from_secs(600),
            cwd: None,
            bare,
        }
    }
}

/// Read-only tool allowlist passed via `--allowed-tools`. Council
/// proposers only need to read and search files — never edit, run shell
/// commands, or fetch the web. Stored as a slice so external callers
/// cannot widen it; the only escape hatch is editing this constant and
/// rebuilding.
const COUNCIL_ALLOWED_TOOLS: &[&str] = &["Read", "Grep", "Glob"];

/// Belt-and-braces denylist. Claude Code's default allow set is "all
/// built-ins", and `--allowed-tools` documentation says it *restricts*
/// the set — but defence in depth: explicitly deny everything dangerous
/// so a future flag-rename or config drift can't accidentally widen the
/// surface.
const COUNCIL_DENIED_TOOLS: &[&str] = &[
    "Bash",
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
];

/// `claude` proxy client. One instance per council run; cheap to clone.
#[derive(Debug, Clone)]
pub struct ClaudeCodeClient {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeClient {
    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self { config }
    }

    /// Build the `claude` argv for a single call. Public for tests so we
    /// can assert the spawn shape without actually invoking `claude`.
    pub(crate) fn build_command(&self, system_prompt: &str) -> Command {
        let mut cmd = Command::new(&self.config.binary);
        cmd.arg("--print")
            .arg("--output-format")
            .arg("json")
            .arg("--input-format")
            .arg("text")
            .arg("--no-session-persistence")
            .arg("--permission-mode")
            .arg("default")
            .arg("--append-system-prompt")
            .arg(system_prompt)
            .arg("--allowed-tools")
            .arg(COUNCIL_ALLOWED_TOOLS.join(" "))
            .arg("--disallowed-tools")
            .arg(COUNCIL_DENIED_TOOLS.join(" "));

        if self.config.bare {
            cmd.arg("--bare");
        }
        if let Some(model) = &self.config.model {
            cmd.arg("--model").arg(model);
        }
        if let Some(budget) = self.config.max_budget_usd {
            cmd.arg("--max-budget-usd").arg(format!("{budget}"));
        }
        if let Some(cwd) = &self.config.cwd {
            cmd.current_dir(cwd);
        }
        cmd
    }
}

impl LlmClient for ClaudeCodeClient {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let t0 = Instant::now();

        // Compose the system prompt from every `Role::System` message
        // (the council passes persona prompt + learned-guidance + AST
        // scope hints separately; we concatenate them with blank-line
        // separators so the model sees one continuous system block) and
        // the user message from the remaining roles. Claude Code takes a
        // single `--append-system-prompt` flag and reads the user
        // payload from stdin.
        let (system_parts, user_parts): (Vec<&String>, Vec<&String>) =
            partition_messages(&req.messages);
        let system_prompt = join_with_blank_lines(&system_parts);
        let user_blob = join_with_blank_lines(&user_parts);

        let mut cmd = self.build_command(&system_prompt);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| LlmError::Transport(format!("spawning {:?}: {e}", self.config.binary)))?;

        // Stream the user message in. Drop the handle so claude sees EOF.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(user_blob.as_bytes())
                .map_err(|e| LlmError::Transport(format!("writing stdin to claude: {e}")))?;
        }

        // Bounded wait. `child.wait_with_output()` doesn't take a
        // timeout on stable Rust, so we poll `try_wait` and `kill` on
        // overrun. Same pattern as `pi_proxy.rs`.
        let deadline = t0 + self.config.timeout;
        let output = loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    break child.wait_with_output().map_err(|e| {
                        LlmError::Transport(format!("collecting claude output: {e}"))
                    })?;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(LlmError::Transport(format!(
                            "claude did not exit within {} seconds",
                            self.config.timeout.as_secs()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(LlmError::Transport(format!("waiting on claude: {e}"))),
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(LlmError::Status {
                status: output.status.code().unwrap_or(1) as u16,
                body: if stderr.is_empty() { stdout } else { stderr },
            });
        }

        // Parse the JSON result envelope. Three failure modes to handle:
        //   1. stdout isn't a Claude-result envelope (older claude,
        //      --output-format mismatch, the model returned bare JSON
        //      findings without the wrapper). Detected via the `type`
        //      discriminator: only `{"type": "result", ...}` payloads
        //      get unwrapped; everything else falls back to the raw
        //      stdout, matching `pi-proxy`'s text-mode behaviour.
        //   2. `is_error: true` in the envelope → surface as Status so
        //      the council captures it into AgentReview.error.
        //   3. `result` is empty → Empty (proposer/judge parsers already
        //      tolerate empty payloads but we'd rather flag the call).
        let parsed: Option<ClaudeResult> = serde_json::from_str(stdout.trim())
            .ok()
            .filter(|r: &ClaudeResult| r.envelope_type.as_deref() == Some("result"));
        let (content_raw, prompt_tokens, completion_tokens) = match parsed {
            Some(r) => {
                if r.is_error.unwrap_or(false) {
                    return Err(LlmError::Status {
                        status: r
                            .api_error_status
                            .and_then(|s| s.parse::<u16>().ok())
                            .unwrap_or(500),
                        body: r.result.unwrap_or_default(),
                    });
                }
                let result_text = r.result.unwrap_or_default();
                let (pt, ct) = r
                    .usage
                    .map(|u| (u.input_tokens, u.output_tokens))
                    .unwrap_or((None, None));
                (result_text, pt, ct)
            }
            None => (stdout, None, None),
        };

        let content = sanitize_text_output(&content_raw);
        if content.is_empty() {
            return Err(LlmError::Empty);
        }

        Ok(ChatResponse {
            content,
            finish_reason: "stop".into(),
            prompt_tokens,
            completion_tokens,
        })
    }
}

/// Subset of `claude --output-format json`'s envelope we actually use.
/// Extra fields are ignored — Anthropic adds telemetry fields between
/// versions and we don't want a parse to fail because the schema grew.
#[derive(Debug, Deserialize)]
struct ClaudeResult {
    /// Discriminator field. Claude Code's `--output-format json` always
    /// emits `"type": "result"`; we require the literal so a bare
    /// `{"findings":[]}` payload (model output without the envelope) is
    /// recognised as not-an-envelope and falls back to raw-stdout.
    #[serde(default, rename = "type")]
    envelope_type: Option<String>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    api_error_status: Option<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    usage: Option<ClaudeUsage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
}

/// Partition a slice of [`ChatMessage`]s into system content vs.
/// everything-else, preserving order within each bucket. Same shape as
/// `pi_proxy::partition_map` but spelled out inline so the two backends
/// don't share an unrelated utility.
fn partition_messages(
    messages: &[aatxe_council::llm::ChatMessage],
) -> (Vec<&String>, Vec<&String>) {
    let mut system = Vec::new();
    let mut user = Vec::new();
    for m in messages {
        match m.role {
            Role::System => system.push(&m.content),
            _ => user.push(&m.content),
        }
    }
    (system, user)
}

fn join_with_blank_lines(parts: &[&String]) -> String {
    let mut acc = String::new();
    for p in parts {
        if !acc.is_empty() {
            acc.push_str("\n\n");
        }
        acc.push_str(p);
    }
    acc
}

/// Strip a single matching markdown fence if present. Mirrors
/// `pi_proxy::sanitize_text_output` so both backends have identical
/// downstream behaviour for the proposer/judge JSON parsers.
fn sanitize_text_output(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        if let Some(nl) = rest.find('\n') {
            let body = &rest[nl + 1..];
            if let Some(stripped) = body.strip_suffix("```") {
                return stripped.trim().to_string();
            }
        }
    }
    trimmed.to_string()
}

// Tests build only on Unix: they install a `#!/bin/sh` shell script as
// a fake-claude binary and chmod it via `std::os::unix::fs::PermissionsExt`.
// Same Unix-only constraint as `pi_proxy.rs`'s test harness.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use aatxe_council::llm::ChatMessage;
    use std::fs;

    /// Compose a tiny shell-script binary that mimics enough of
    /// `claude`'s `--print` surface for unit tests. The script:
    ///   * echoes a canned response (passed as `canned_stdout`) to stdout
    ///   * exits with `exit_code`
    ///   * writes the invocation argv + stdin to a sidecar file so the
    ///     test can assert what `ClaudeCodeClient` actually called.
    fn fake_claude(canned_stdout: &str, exit_code: i32) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("claude");
        let capture = dir.path().join("capture.txt");
        let script = format!(
            "#!/bin/sh\n\
             {{ echo \"argv: $@\"; echo '---stdin---'; cat; }} > {cap}\n\
             printf '%s' {payload}\n\
             exit {code}\n",
            cap = shell_escape(capture.to_str().unwrap()),
            payload = shell_escape(canned_stdout),
            code = exit_code
        );
        fs::write(&bin, script).expect("write fake-claude");
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
        (dir, bin, capture)
    }

    fn shell_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for c in s.chars() {
            if c == '\'' {
                out.push_str("'\\''");
            } else {
                out.push(c);
            }
        }
        out.push('\'');
        out
    }

    fn cfg_with(bin: PathBuf) -> ClaudeCodeConfig {
        ClaudeCodeConfig {
            binary: bin,
            model: None,
            max_budget_usd: None,
            timeout: Duration::from_secs(5),
            cwd: None,
            // Tests assert `--bare` is plumbed correctly; production
            // default is `false` so OAuth/keychain auth works on a
            // fresh laptop. See `from_env` doc-comment.
            bare: true,
        }
    }

    /// Build a Claude-result JSON envelope of the shape `claude
    /// --output-format json` actually emits, so the test asserts the
    /// production parsing path (not a stub of it).
    fn result_envelope(result: &str, input_tokens: u32, output_tokens: u32) -> String {
        format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"result":{r},"duration_ms":1,"total_cost_usd":0.01,"usage":{{"input_tokens":{i},"output_tokens":{o}}}}}"#,
            r = serde_json::to_string(result).unwrap(),
            i = input_tokens,
            o = output_tokens,
        )
    }

    #[test]
    fn build_command_passes_council_tool_allowlist_and_denylist() {
        let (_dir, bin, _cap) = fake_claude("{}", 0);
        let client = ClaudeCodeClient::new(cfg_with(bin));
        let cmd = client.build_command("SYS");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        // Print mode + JSON output + no session persistence are
        // load-bearing for "one-shot stateless call".
        for required in [
            "--print",
            "--output-format",
            "json",
            "--no-session-persistence",
            "--permission-mode",
            "default",
            "--bare",
        ] {
            assert!(
                argv.iter().any(|a| a == required),
                "missing required flag/value: {required}, argv={argv:?}"
            );
        }

        // The tool list must be exactly the read-only set, in fixed order,
        // so a future config bug can't quietly widen it.
        let pos = argv
            .iter()
            .position(|a| a == "--allowed-tools")
            .expect("--allowed-tools missing");
        assert_eq!(argv[pos + 1], "Read Grep Glob");

        // The denylist must include every dangerous built-in.
        let pos = argv
            .iter()
            .position(|a| a == "--disallowed-tools")
            .expect("--disallowed-tools missing");
        let denied = &argv[pos + 1];
        for forbidden in ["Bash", "Edit", "Write", "WebFetch", "WebSearch"] {
            assert!(
                denied.contains(forbidden),
                "denylist missing {forbidden}: {denied}"
            );
        }
    }

    #[test]
    fn build_command_propagates_model_and_budget_when_set() {
        let (_dir, bin, _cap) = fake_claude("{}", 0);
        let mut cfg = cfg_with(bin);
        cfg.model = Some("opus".into());
        cfg.max_budget_usd = Some(2.5);
        let client = ClaudeCodeClient::new(cfg);
        let cmd = client.build_command("SYS");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let mpos = argv.iter().position(|a| a == "--model").expect("--model");
        assert_eq!(argv[mpos + 1], "opus");
        let bpos = argv
            .iter()
            .position(|a| a == "--max-budget-usd")
            .expect("--max-budget-usd");
        assert_eq!(argv[bpos + 1], "2.5");
    }

    #[test]
    fn build_command_omits_optional_flags_when_unset() {
        let (_dir, bin, _cap) = fake_claude("{}", 0);
        let client = ClaudeCodeClient::new(cfg_with(bin));
        let cmd = client.build_command("SYS");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!argv.iter().any(|a| a == "--model"));
        assert!(!argv.iter().any(|a| a == "--max-budget-usd"));
    }

    #[test]
    fn chat_routes_system_to_flag_user_to_stdin_and_extracts_result() {
        let canned = result_envelope(r#"{"findings":[]}"#, 100, 20);
        let (_dir, bin, cap) = fake_claude(&canned, 0);
        let client = ClaudeCodeClient::new(cfg_with(bin));
        let resp = client
            .chat(ChatRequest {
                model: "ignored".into(),
                messages: vec![
                    ChatMessage::system("PERSONA-XYZ"),
                    ChatMessage::user("USER-BLOB-HERE"),
                ],
                temperature: 0.2,
                max_tokens: 256,
                json_only: true,
            })
            .expect("chat");
        assert_eq!(resp.content, r#"{"findings":[]}"#);
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.prompt_tokens, Some(100));
        assert_eq!(resp.completion_tokens, Some(20));
        let captured = fs::read_to_string(&cap).unwrap();
        assert!(
            captured.contains("PERSONA-XYZ"),
            "system prompt should appear in argv: {captured}"
        );
        assert!(
            captured.contains("USER-BLOB-HERE"),
            "user message should appear in stdin: {captured}"
        );
    }

    #[test]
    fn chat_returns_status_error_on_nonzero_exit() {
        let (_dir, bin, _cap) = fake_claude("boom", 17);
        let client = ClaudeCodeClient::new(cfg_with(bin));
        let err = client
            .chat(ChatRequest {
                model: "ignored".into(),
                messages: vec![ChatMessage::system("S"), ChatMessage::user("U")],
                temperature: 0.0,
                max_tokens: 1,
                json_only: false,
            })
            .unwrap_err();
        match err {
            LlmError::Status { status, body } => {
                assert_eq!(status, 17);
                assert!(body.contains("boom"), "body={body}");
            }
            other => panic!("expected Status error, got {other:?}"),
        }
    }

    #[test]
    fn chat_surfaces_envelope_is_error_as_status() {
        // claude exits 0 but reports is_error=true in the envelope. We
        // map this to LlmError::Status so the council's fail-soft path
        // captures it into AgentReview.error rather than silently
        // returning a truncated result.
        let envelope = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"api_error_status":"529","result":"upstream overloaded","duration_ms":1}"#;
        let (_dir, bin, _cap) = fake_claude(envelope, 0);
        let client = ClaudeCodeClient::new(cfg_with(bin));
        let err = client
            .chat(ChatRequest {
                model: "ignored".into(),
                messages: vec![ChatMessage::system("S"), ChatMessage::user("U")],
                temperature: 0.0,
                max_tokens: 1,
                json_only: false,
            })
            .unwrap_err();
        match err {
            LlmError::Status { status, body } => {
                assert_eq!(status, 529);
                assert!(body.contains("upstream overloaded"), "body={body}");
            }
            other => panic!("expected Status error, got {other:?}"),
        }
    }

    #[test]
    fn chat_returns_empty_error_on_blank_result() {
        let canned = result_envelope("", 0, 0);
        let (_dir, bin, _cap) = fake_claude(&canned, 0);
        let client = ClaudeCodeClient::new(cfg_with(bin));
        let err = client
            .chat(ChatRequest {
                model: "ignored".into(),
                messages: vec![ChatMessage::system("S"), ChatMessage::user("U")],
                temperature: 0.0,
                max_tokens: 1,
                json_only: false,
            })
            .unwrap_err();
        assert!(matches!(err, LlmError::Empty));
    }

    #[test]
    fn chat_falls_back_to_raw_stdout_when_not_json() {
        // Older claude / mismatched --output-format: stdout isn't JSON.
        // We accept it as raw text rather than failing — the proposer
        // parser tolerates prose around JSON.
        let (_dir, bin, _cap) = fake_claude(r#"{"findings":[]}"#, 0);
        let client = ClaudeCodeClient::new(cfg_with(bin));
        let resp = client
            .chat(ChatRequest {
                model: "ignored".into(),
                messages: vec![ChatMessage::system("S"), ChatMessage::user("U")],
                temperature: 0.0,
                max_tokens: 1,
                json_only: true,
            })
            .expect("chat");
        // Raw stdout was a JSON findings doc, not a Claude envelope:
        // the client recognises it as not-an-envelope and forwards it
        // verbatim. Token telemetry is None on this fallback path.
        assert_eq!(resp.content, r#"{"findings":[]}"#);
        assert!(resp.prompt_tokens.is_none());
        assert!(resp.completion_tokens.is_none());
    }

    #[test]
    fn chat_strips_a_matching_markdown_fence_in_result() {
        let canned = result_envelope("```json\n{\"findings\":[]}\n```", 5, 6);
        let (_dir, bin, _cap) = fake_claude(&canned, 0);
        let client = ClaudeCodeClient::new(cfg_with(bin));
        let resp = client
            .chat(ChatRequest {
                model: "ignored".into(),
                messages: vec![ChatMessage::system("S"), ChatMessage::user("U")],
                temperature: 0.0,
                max_tokens: 1,
                json_only: true,
            })
            .unwrap();
        assert_eq!(resp.content, r#"{"findings":[]}"#);
        assert_eq!(resp.prompt_tokens, Some(5));
        assert_eq!(resp.completion_tokens, Some(6));
    }

    #[test]
    fn spawn_failure_is_transport_error() {
        let mut cfg = cfg_with(PathBuf::from("/does/not/exist/claude-xyz"));
        cfg.timeout = Duration::from_secs(1);
        let client = ClaudeCodeClient::new(cfg);
        let err = client
            .chat(ChatRequest {
                model: "ignored".into(),
                messages: vec![ChatMessage::system("S"), ChatMessage::user("U")],
                temperature: 0.0,
                max_tokens: 1,
                json_only: false,
            })
            .unwrap_err();
        assert!(matches!(err, LlmError::Transport(_)));
    }

    #[test]
    fn timeout_kills_the_child_and_returns_transport_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("claude");
        fs::write(&bin, "#!/bin/sh\nsleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut p = fs::metadata(&bin).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(&bin, p).unwrap();

        let mut cfg = cfg_with(bin);
        cfg.timeout = Duration::from_millis(300);
        let client = ClaudeCodeClient::new(cfg);
        let t0 = Instant::now();
        let err = client
            .chat(ChatRequest {
                model: "ignored".into(),
                messages: vec![ChatMessage::system("S"), ChatMessage::user("U")],
                temperature: 0.0,
                max_tokens: 1,
                json_only: false,
            })
            .unwrap_err();
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "timeout did not fire promptly: {elapsed:?}"
        );
        match err {
            LlmError::Transport(msg) => assert!(
                msg.contains("did not exit"),
                "unexpected transport msg: {msg}"
            ),
            other => panic!("expected Transport timeout, got {other:?}"),
        }
    }

    #[test]
    fn from_env_picks_up_overrides() {
        // Sanity: CLAUDE_BIN / CLAUDE_MODEL / CLAUDE_MAX_BUDGET_USD /
        // CLAUDE_BARE all round-trip through ClaudeCodeConfig::from_env.
        // The env touched here isn't read elsewhere in the suite.
        std::env::set_var("CLAUDE_BIN", "/usr/local/bin/claude-test");
        std::env::set_var("CLAUDE_MODEL", "sonnet");
        std::env::set_var("CLAUDE_MAX_BUDGET_USD", "0.75");
        std::env::set_var("CLAUDE_BARE", "1");
        let cfg = ClaudeCodeConfig::from_env();
        assert_eq!(cfg.binary, PathBuf::from("/usr/local/bin/claude-test"));
        assert_eq!(cfg.model.as_deref(), Some("sonnet"));
        assert_eq!(cfg.max_budget_usd, Some(0.75));
        assert!(cfg.bare, "CLAUDE_BARE=1 must flip bare on");
        std::env::remove_var("CLAUDE_BIN");
        std::env::remove_var("CLAUDE_MODEL");
        std::env::remove_var("CLAUDE_MAX_BUDGET_USD");
        std::env::remove_var("CLAUDE_BARE");

        // And the default is OAuth-friendly: bare=false, so a fresh
        // laptop with subscription auth works without setting any env.
        let cfg = ClaudeCodeConfig::from_env();
        assert!(
            !cfg.bare,
            "default bare must be false to let OAuth/keychain auth work"
        );
    }

    #[test]
    fn sanitize_strips_only_fenced_blocks() {
        assert_eq!(sanitize_text_output("plain"), "plain");
        assert_eq!(sanitize_text_output("   plain   "), "plain");
        assert_eq!(
            sanitize_text_output("```json\n{\"a\":1}\n```"),
            r#"{"a":1}"#
        );
    }
}
