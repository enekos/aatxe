//! `pi` proxy — the council's only LLM backend.
//!
//! Spawns the locally-installed [`pi`](https://github.com/onepingsoftware/pi-agent)
//! coding-agent CLI per LLM call and treats it as a tool-using proxy in
//! front of the underlying model. `pi` lets the model interactively
//! `read`, `grep`, and `find` files in the repo under review — so the
//! council isn't a pure context-packer; it asks the agent to fetch what
//! it needs.
//!
//! Earlier versions of aatxe had a direct-HTTP backend
//! (`KimiClient` → `api.moonshot.ai`); it was removed because the only
//! Kimi API key available to aatxe runs against `api.kimi.com`, which
//! enforces a user-agent allowlist (Kimi CLI / Claude Code / Roo Code /
//! Kilo Code / `pi`) and 403s every other client. Routing through `pi`
//! is therefore the only working path; the spec'd "two backends, opt in
//! one" design collapsed to one.
//!
//! ## Crate boundary
//!
//! `PiAgentClient` implements [`LlmClient`] so the pure `aatxe-council`
//! crate stays unaware of process spawning. The `aatxe` binary owns the
//! `Command`; the pipeline keeps the same trait seam.
//!
//! ## Safety surface
//!
//! `pi` ships seven built-in tools: `read`, `bash`, `edit`, `write`, `grep`,
//! `find`, `ls`. The council *only* needs read-only ones; we hardcode the
//! allowlist at construction time and never let an external caller widen
//! it. `bash`/`edit`/`write` are categorically off — there is no flag, no
//! env var, and no `CouncilOptions` field that turns them on. If you find
//! yourself wanting them, write a new client; don't extend this one.
//!
//! ## Output shape
//!
//! We invoke `pi --print --mode text` which writes the final assistant
//! reply (and nothing else) to stdout. The proposer/judge system prompts
//! demand strict-JSON answers, so stdout is the JSON object the existing
//! parser already handles. Token telemetry is dropped on this path (text
//! mode doesn't surface usage); the CLI's per-call duration is still
//! captured by the pipeline.
//!
//! ## Latency
//!
//! `pi` spins up a Node runtime per invocation. On an M-series Mac the
//! cold-start is ~700 ms, plus the underlying model latency (~0.6–4 s per
//! turn, multiplied by however many tool turns the agent takes). The
//! pipeline already parallelises proposers across personas, so wall-clock
//! is roughly the slowest single agent call rather than 4×.

use aatxe_core::secret::Secret;
use aatxe_council::llm::{ChatRequest, ChatResponse, LlmClient, LlmError, Role};
use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Where to find the `pi` binary, which model to drive it with, and how
/// long to wait. Defaults match a freshly-installed `pi` on macOS plus
/// `KIMI_API_KEY` already in the environment.
#[derive(Debug, Clone)]
pub struct PiConfig {
    /// Path or executable name to invoke. Defaults to `"pi"` (looked up on
    /// `$PATH`).
    pub binary: PathBuf,
    /// `pi --provider <…>`. Defaults to `kimi-coding` since `KIMI_API_KEY`
    /// is the council's default credential.
    pub provider: String,
    /// `pi --model <…>`. Defaults to `kimi-k2-thinking`.
    pub model: String,
    /// Wall-clock budget for one invocation. Defaults to 10 minutes —
    /// `pi` cold-starts a Node runtime per call, and thinking-class
    /// models (e.g. `kimi-k2-thinking`) routinely take 3–7 minutes per
    /// proposer when the agent burns tool turns chasing references
    /// (empirically measured: correctness persona on a 728-LOC diff hits
    /// the 8-min ceiling).
    pub timeout: Duration,
    /// Working directory the `pi` subprocess runs in. The agent's
    /// `read`/`grep`/`find` are rooted here, so this should be the repo
    /// being reviewed. Defaults to the parent process's cwd.
    pub cwd: Option<PathBuf>,
    /// Forward `KIMI_API_KEY` (and friends) to the child? Defaults to
    /// `true`. Off only in tests.
    pub forward_kimi_env: bool,
}

impl PiConfig {
    /// Discover from environment. Reads three env vars: `PI_BIN`
    /// (overrides the executable path — handy for fnm-style multi-version
    /// setups where `pi` isn't on the system `$PATH`), `PI_PROVIDER`,
    /// and `PI_MODEL`. The remaining fields (`timeout`, `cwd`,
    /// `forward_kimi_env`) always take compiled-in defaults; there is no
    /// `PI_TIMEOUT` / `PI_CWD` knob.
    pub fn from_env() -> Self {
        let binary = env::var("PI_BIN")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("pi"));
        let provider = env::var("PI_PROVIDER").unwrap_or_else(|_| "kimi-coding".into());
        let model = env::var("PI_MODEL").unwrap_or_else(|_| "kimi-k2-thinking".into());
        Self {
            binary,
            provider,
            model,
            timeout: Duration::from_secs(600),
            cwd: None,
            forward_kimi_env: true,
        }
    }
}

/// Fixed, hardcoded tool allowlist for council use. This is read-only by
/// construction — see the module-level "Safety surface" note. Stored as
/// a slice rather than a config field so external callers cannot widen
/// it; the only escape hatch is editing this constant and rebuilding.
const COUNCIL_TOOLS: &[&str] = &["read", "grep", "find", "ls"];

/// `pi` proxy client. One instance per council run; cheap to clone (the
/// inner [`Secret`] holds the env-forwarded `KIMI_API_KEY` and is `Arc`
/// internally).
#[derive(Debug, Clone)]
pub struct PiAgentClient {
    config: PiConfig,
    /// Optional override of the env-discovered Kimi key. When `None`, the
    /// child process inherits the parent's `KIMI_API_KEY` directly.
    api_key: Option<Secret>,
}

impl PiAgentClient {
    pub fn new(config: PiConfig) -> Self {
        let api_key = env::var("KIMI_API_KEY").ok().map(Secret::new);
        Self { config, api_key }
    }

    /// Builder-style override of the underlying API key. Used by the test
    /// suite (which doesn't want the real key in scope) and by future
    /// callers that source credentials from somewhere other than `env`.
    #[allow(dead_code)]
    pub fn with_api_key(mut self, key: Option<Secret>) -> Self {
        self.api_key = key;
        self
    }

    /// Build the `pi` argv for a single call. Public for tests so we can
    /// assert the spawn shape without actually running `pi`.
    pub(crate) fn build_command(&self, system_prompt: &str) -> Command {
        let mut cmd = Command::new(&self.config.binary);
        cmd.arg("--print")
            .arg("--mode")
            .arg("text")
            .arg("--no-session")
            .arg("--provider")
            .arg(&self.config.provider)
            .arg("--model")
            .arg(&self.config.model)
            .arg("--tools")
            .arg(COUNCIL_TOOLS.join(","))
            .arg("--system-prompt")
            .arg(system_prompt);

        if let Some(cwd) = &self.config.cwd {
            cmd.current_dir(cwd);
        }
        // Environment hygiene: the child inherits PATH + HOME + KIMI_API_KEY
        // by default. We don't strip the rest of the env because `pi`
        // legitimately reads PI_OFFLINE, PI_TELEMETRY, fnm/asdf shims, etc.
        // — stripping would be fragile across user setups.
        if !self.config.forward_kimi_env {
            cmd.env_remove("KIMI_API_KEY");
        } else if let Some(key) = &self.api_key {
            cmd.env("KIMI_API_KEY", key.reveal());
        }
        cmd
    }
}

impl LlmClient for PiAgentClient {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let t0 = Instant::now();

        // Compose the system prompt from every `Role::System` message,
        // and the user message from the remaining non-system roles. `pi`
        // takes a single `--system-prompt` string and reads the user
        // payload from stdin; assistant turns (which the council never
        // sends today) would be inlined into the user blob.
        let (system_parts, user_parts): (Vec<&String>, Vec<&String>) =
            req.messages.iter().partition_map(|m| match m.role {
                Role::System => either::Either::Left(&m.content),
                _ => either::Either::Right(&m.content),
            });
        let system_prompt = system_parts.iter().fold(String::new(), |mut acc, s| {
            if !acc.is_empty() {
                acc.push_str("\n\n");
            }
            acc.push_str(s);
            acc
        });
        let user_blob = user_parts.iter().fold(String::new(), |mut acc, s| {
            if !acc.is_empty() {
                acc.push_str("\n\n");
            }
            acc.push_str(s);
            acc
        });

        let mut cmd = self.build_command(&system_prompt);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| LlmError::Transport(format!("spawning {:?}: {e}", self.config.binary)))?;

        // Stream the user message in. Drop the handle so `pi` sees EOF.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(user_blob.as_bytes())
                .map_err(|e| LlmError::Transport(format!("writing stdin to pi: {e}")))?;
        }

        // Bounded wait. `child.wait_with_output()` doesn't take a timeout
        // on stable Rust, so we poll `try_wait` and `kill` on overrun.
        let deadline = t0 + self.config.timeout;
        let output = loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    // Process exited — drain stdout/stderr.
                    break child
                        .wait_with_output()
                        .map_err(|e| LlmError::Transport(format!("collecting pi output: {e}")))?;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(LlmError::Transport(format!(
                            "pi did not exit within {} seconds",
                            self.config.timeout.as_secs()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(LlmError::Transport(format!("waiting on pi: {e}"))),
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            // Treat any non-zero exit as a transport-tier failure so the
            // council's fail-soft path captures it into AgentReview.error
            // and the surviving proposers keep running.
            return Err(LlmError::Status {
                status: output.status.code().unwrap_or(1) as u16,
                body: if stderr.is_empty() { stdout } else { stderr },
            });
        }

        let content = sanitize_text_output(&stdout);
        if content.is_empty() {
            return Err(LlmError::Empty);
        }

        Ok(ChatResponse {
            content,
            finish_reason: "stop".into(),
            // text-mode output drops usage metadata. The pipeline's
            // per-agent `duration_ms` still gets recorded.
            prompt_tokens: None,
            completion_tokens: None,
        })
    }
}

/// Some pi providers (notably the Claude-family ones via `pi --mode text`)
/// occasionally wrap JSON answers in a markdown ```json fence even after
/// being told not to. The proposer/judge parsers already tolerate prose
/// around the JSON object, but stripping a single matching fence here is
/// cheap insurance and keeps every other backend's behaviour identical.
///
/// We *also* trim trailing/leading whitespace so the parser sees a clean
/// `{` … `}` boundary.
fn sanitize_text_output(raw: &str) -> String {
    let trimmed = raw.trim();
    // Strip a leading ```<lang> fence and the matching ``` if present.
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Drop the first line (the fence + optional language tag).
        if let Some(nl) = rest.find('\n') {
            let body = &rest[nl + 1..];
            if let Some(stripped) = body.strip_suffix("```") {
                return stripped.trim().to_string();
            }
            // Fence opened but didn't close → fall through to raw return.
        }
    }
    trimmed.to_string()
}

// ---------------------------------------------------------------------------
// `itertools::Itertools::partition_map` brings in a 50KB dep for one call.
// Roll a tiny local equivalent on the std iterator instead.
mod either {
    pub enum Either<L, R> {
        Left(L),
        Right(R),
    }
}

trait PartitionMap<T> {
    fn partition_map<L, R, F>(self, f: F) -> (Vec<L>, Vec<R>)
    where
        Self: Sized,
        F: FnMut(T) -> either::Either<L, R>;
}

impl<I: Iterator<Item = T>, T> PartitionMap<T> for I {
    fn partition_map<L, R, F>(self, mut f: F) -> (Vec<L>, Vec<R>)
    where
        F: FnMut(T) -> either::Either<L, R>,
    {
        let mut left = Vec::new();
        let mut right = Vec::new();
        for item in self {
            match f(item) {
                either::Either::Left(l) => left.push(l),
                either::Either::Right(r) => right.push(r),
            }
        }
        (left, right)
    }
}

// Tests build only on Unix: they install a `#!/bin/sh` shell script as a
// fake-pi binary and chmod it via `std::os::unix::fs::PermissionsExt`.
// The production code in this module is portable; only the test harness
// is Unix-specific. A future cross-platform test path would need to
// install a `.bat` / `.exe` stand-in on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use aatxe_council::llm::ChatMessage;
    use std::fs;

    /// Compose a tiny shell-script binary that mimics enough of `pi`'s
    /// surface for unit tests. The script:
    ///   * echoes a canned response (passed as $1) to stdout
    ///   * exits with the given status (passed as $2)
    ///   * writes the invocation argv + stdin to a sidecar file so the
    ///     test can assert what `PiAgentClient` actually called.
    ///
    /// Returns the path of the fake binary + the path of the capture
    /// file. Caller owns both via the returned `tempfile::TempDir`.
    fn fake_pi(canned_stdout: &str, exit_code: i32) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("pi");
        let capture = dir.path().join("capture.txt");
        // The script ignores all flags and just echoes what the test
        // asks. It records `$@` and stdin into the capture file so the
        // unit tests can assert PiAgentClient passed the right argv.
        let script = format!(
            "#!/bin/sh\n\
             {{ echo \"argv: $@\"; echo '---stdin---'; cat; }} > {cap}\n\
             printf '%s' {payload}\n\
             exit {code}\n",
            cap = shell_escape(capture.to_str().unwrap()),
            payload = shell_escape(canned_stdout),
            code = exit_code
        );
        fs::write(&bin, script).expect("write fake-pi");
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

    fn cfg_with(bin: PathBuf) -> PiConfig {
        PiConfig {
            binary: bin,
            provider: "kimi-coding".into(),
            model: "kimi-k2-thinking".into(),
            timeout: Duration::from_secs(5),
            cwd: None,
            forward_kimi_env: false,
        }
    }

    #[test]
    fn build_command_passes_council_tool_allowlist_and_no_session() {
        let (_dir, bin, _cap) = fake_pi("{}", 0);
        let client = PiAgentClient::new(cfg_with(bin)).with_api_key(None);
        let cmd = client.build_command("SYS");
        // `Command::get_args` returns OsStr; collect to strings for easy
        // assertions.
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(argv.iter().any(|a| a == "--no-session"));
        assert!(argv.iter().any(|a| a == "--print"));
        // The tool list must be exactly the read-only set, in fixed order,
        // so a future config bug can't quietly widen it.
        let pos = argv
            .iter()
            .position(|a| a == "--tools")
            .expect("--tools missing");
        assert_eq!(argv[pos + 1], "read,grep,find,ls");
        // bash/edit/write must never appear anywhere in the argv.
        for forbidden in ["bash", "edit", "write"] {
            assert!(
                !argv.iter().any(|a| a == forbidden),
                "{forbidden} leaked into argv: {argv:?}"
            );
        }
    }

    #[test]
    fn chat_routes_system_messages_to_flag_and_user_to_stdin() {
        let (_dir, bin, cap) = fake_pi(r#"{"findings":[]}"#, 0);
        let client = PiAgentClient::new(cfg_with(bin)).with_api_key(None);
        let req = ChatRequest {
            model: "ignored".into(),
            messages: vec![
                ChatMessage::system("PERSONA-XYZ"),
                ChatMessage::user("USER-BLOB-HERE"),
            ],
            temperature: 0.2,
            max_tokens: 256,
            json_only: true,
        };
        let resp = client.chat(req).expect("chat");
        assert_eq!(resp.content, r#"{"findings":[]}"#);
        assert_eq!(resp.finish_reason, "stop");
        assert!(resp.prompt_tokens.is_none());
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
        let (_dir, bin, _cap) = fake_pi("boom", 17);
        let client = PiAgentClient::new(cfg_with(bin)).with_api_key(None);
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
                // Body holds stderr when present, stdout otherwise. The
                // fake doesn't write stderr → body should carry stdout.
                assert!(body.contains("boom"), "body={body}");
            }
            other => panic!("expected Status error, got {other:?}"),
        }
    }

    #[test]
    fn chat_returns_empty_error_on_blank_stdout() {
        let (_dir, bin, _cap) = fake_pi("", 0);
        let client = PiAgentClient::new(cfg_with(bin)).with_api_key(None);
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
    fn chat_strips_a_matching_markdown_fence() {
        let (_dir, bin, _cap) = fake_pi("```json\n{\"findings\":[]}\n```", 0);
        let client = PiAgentClient::new(cfg_with(bin)).with_api_key(None);
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
    }

    #[test]
    fn spawn_failure_is_transport_error() {
        // Non-existent binary → Command::spawn fails before any IO.
        let mut cfg = cfg_with(PathBuf::from("/does/not/exist/pi-xyz"));
        cfg.timeout = Duration::from_secs(1);
        let client = PiAgentClient::new(cfg).with_api_key(None);
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
        // A fake-pi that sleeps longer than our timeout.
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("pi");
        fs::write(&bin, "#!/bin/sh\nsleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut p = fs::metadata(&bin).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(&bin, p).unwrap();

        let mut cfg = cfg_with(bin);
        cfg.timeout = Duration::from_millis(300);
        let client = PiAgentClient::new(cfg).with_api_key(None);
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
    fn sanitize_strips_only_fenced_json_blocks() {
        assert_eq!(sanitize_text_output("plain"), "plain");
        assert_eq!(sanitize_text_output("   plain   "), "plain");
        assert_eq!(
            sanitize_text_output("```json\n{\"a\":1}\n```"),
            r#"{"a":1}"#
        );
        // Unterminated fence — leave as-is (parser downstream will deal).
        assert_eq!(
            sanitize_text_output("```json\n{\"a\":1}\n").trim_end(),
            "```json\n{\"a\":1}"
        );
    }

    #[test]
    fn partition_map_routes_messages_to_left_and_right() {
        let items = vec!["s1", "u1", "s2", "u2"];
        let (sys, user): (Vec<&str>, Vec<&str>) = items.into_iter().partition_map(|m| {
            if m.starts_with('s') {
                either::Either::Left(m)
            } else {
                either::Either::Right(m)
            }
        });
        assert_eq!(sys, vec!["s1", "s2"]);
        assert_eq!(user, vec!["u1", "u2"]);
    }
}
