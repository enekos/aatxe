//! Agent runners: turn a task prompt + worktree into a stream of
//! [`AgentOutputKind`] events and an exit code.
//!
//! Two backends, same pattern as the council's `LlmClient` seam:
//!
//! * `ClaudeCode` shells out to the local `claude` CLI in print mode with
//!   `--output-format stream-json`, exactly how aatxe already shells out
//!   to `npx aatxe-ts-runner` or `go test -bench`. The agent edits code
//!   for real — it gets `Edit`/`Write`/`Bash` — but only inside its own
//!   detached worktree, never the main checkout.
//! * `Stub` is the deterministic offline runner: it appends lines to a
//!   scratch file on a timer, which drives the dirty-poll → bench loop
//!   end-to-end with zero network and zero LLM spend. Used by tests and
//!   by `aatxe ui --agent-backend stub` demo runs.
//! * `Gemini` is a *native* tool-use loop over the Gemini API (no local
//!   agent CLI exists for it) — see [`crate::gemini`]. One
//!   `GEMINI_API_KEY` drives both this and the council's gemini backend.

use crate::events::AgentOutputKind;
use crate::gemini::{run_gemini_agent, GeminiAgentConfig};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Callback used to surface agent activity while the process runs.
pub type EmitFn = Arc<dyn Fn(AgentOutputKind, String) + Send + Sync>;

#[derive(Debug, Clone)]
pub enum AgentBackend {
    ClaudeCode {
        binary: PathBuf,
        model: Option<String>,
        allowed_tools: Vec<String>,
    },
    Gemini(GeminiAgentConfig),
    Stub {
        edits: u32,
        sleep_ms: u64,
    },
}

/// The default tool surface for a coding agent. Wider than the council's
/// read-only set on purpose: the agent's whole job is to edit and build,
/// and it is confined to a throwaway worktree on a dedicated branch.
pub fn default_allowed_tools() -> Vec<String> {
    ["Read", "Grep", "Glob", "Edit", "Write", "Bash"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// Build the `claude` argv for one agent run. Split out for tests.
pub(crate) fn build_claude_command(
    binary: &Path,
    model: Option<&str>,
    allowed_tools: &[String],
    task: &str,
    worktree: &Path,
) -> Command {
    let mut cmd = Command::new(binary);
    cmd.arg("--print")
        .arg(task)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--permission-mode")
        .arg("acceptEdits")
        .arg("--no-session-persistence")
        .arg("--allowed-tools")
        .arg(allowed_tools.join(" "));
    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }
    cmd.current_dir(worktree);
    cmd
}

/// Run the agent to completion, forwarding activity through `emit`.
/// Returns the process exit code (`None` if killed by signal; the stub
/// always returns `Some(0)`).
pub async fn run_agent(
    backend: &AgentBackend,
    task: &str,
    worktree: &Path,
    emit: EmitFn,
) -> Result<Option<i32>> {
    match backend {
        AgentBackend::Stub { edits, sleep_ms } => {
            run_stub(*edits, *sleep_ms, worktree, emit).await?;
            Ok(Some(0))
        }
        AgentBackend::Gemini(cfg) => {
            // The Gemini loop is blocking (ureq + std::process), matching
            // the rest of aatxe; isolate it on the blocking pool.
            let cfg = cfg.clone();
            let task = task.to_string();
            let wt = worktree.to_path_buf();
            tokio::task::spawn_blocking(move || run_gemini_agent(&cfg, &task, &wt, emit))
                .await
                .map_err(|e| anyhow!("gemini agent task panicked: {e}"))??;
            Ok(Some(0))
        }
        AgentBackend::ClaudeCode {
            binary,
            model,
            allowed_tools,
        } => {
            let mut cmd =
                build_claude_command(binary, model.as_deref(), allowed_tools, task, worktree);
            let mut child = cmd
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("spawning {}", binary.display()))?;

            let stderr = child.stderr.take().expect("stderr piped");
            let emit_err = emit.clone();
            let stderr_task = tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !line.trim().is_empty() {
                        emit_err(AgentOutputKind::Stderr, line);
                    }
                }
            });

            let stdout = child.stdout.take().expect("stdout piped");
            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines.next_line().await.context("reading agent stdout")? {
                for (kind, text) in parse_stream_line(&line) {
                    emit(kind, text);
                }
            }
            let status = child.wait().await.context("waiting for agent")?;
            let _ = stderr_task.await;
            Ok(status.code())
        }
    }
}

async fn run_stub(edits: u32, sleep_ms: u64, worktree: &Path, emit: EmitFn) -> Result<()> {
    emit(
        AgentOutputKind::System,
        "stub agent started — scripted edits, no LLM".into(),
    );
    let scratch = worktree.join("AATXE_UI_STUB.md");
    for i in 1..=edits {
        tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
        let mut body = std::fs::read_to_string(&scratch).unwrap_or_default();
        body.push_str(&format!("stub edit {i}\n"));
        std::fs::write(&scratch, body).context("writing stub scratch file")?;
        emit(AgentOutputKind::ToolUse, "Write AATXE_UI_STUB.md".into());
        emit(AgentOutputKind::Text, format!("stub edit {i} of {edits}"));
    }
    emit(AgentOutputKind::Text, "stub agent done".into());
    Ok(())
}

/// Parse one `--output-format stream-json` NDJSON line into zero or more
/// display events. Tolerant by design: an unrecognized or non-JSON line
/// degrades to raw text, never an error — the harness must survive
/// stream-format drift between `claude` versions.
pub(crate) fn parse_stream_line(line: &str) -> Vec<(AgentOutputKind, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return vec![(AgentOutputKind::Text, trimmed.to_string())];
    };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("system") => {
            let subtype = v.get("subtype").and_then(|s| s.as_str()).unwrap_or("event");
            vec![(AgentOutputKind::System, format!("system: {subtype}"))]
        }
        Some("assistant") => {
            let mut out = Vec::new();
            if let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                for block in content {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                if !text.trim().is_empty() {
                                    out.push((AgentOutputKind::Text, text.to_string()));
                                }
                            }
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            let input = block
                                .get("input")
                                .map(|i| truncate(&i.to_string(), 160))
                                .unwrap_or_default();
                            out.push((AgentOutputKind::ToolUse, format!("{name} {input}")));
                        }
                        _ => {}
                    }
                }
            }
            out
        }
        Some("user") => {
            let mut out = Vec::new();
            if let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        let text = tool_result_text(block);
                        if !text.is_empty() {
                            out.push((AgentOutputKind::ToolResult, truncate(&text, 200)));
                        }
                    }
                }
            }
            out
        }
        Some("result") => {
            let subtype = v.get("subtype").and_then(|s| s.as_str()).unwrap_or("done");
            let cost = v
                .get("total_cost_usd")
                .and_then(|c| c.as_f64())
                .map(|c| format!(" · ${c:.4}"))
                .unwrap_or_default();
            vec![(AgentOutputKind::System, format!("result: {subtype}{cost}"))]
        }
        _ => vec![(AgentOutputKind::Text, truncate(trimmed, 200))],
    }
}

/// `tool_result` content is either a plain string or an array of
/// `{type: "text", text}` blocks. Handle both.
fn tool_result_text(block: &serde_json::Value) -> String {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Char-boundary-safe truncation with an ellipsis marker.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_argv_has_stream_json_and_tool_allowlist() {
        let cmd = build_claude_command(
            Path::new("claude"),
            Some("opus"),
            &default_allowed_tools(),
            "fix the bug",
            Path::new("/tmp/wt"),
        );
        let argv: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for required in [
            "--print",
            "fix the bug",
            "stream-json",
            "--verbose",
            "acceptEdits",
            "--no-session-persistence",
        ] {
            assert!(
                argv.iter().any(|a| a == required),
                "missing {required}: {argv:?}"
            );
        }
        let pos = argv.iter().position(|a| a == "--allowed-tools").unwrap();
        assert_eq!(argv[pos + 1], "Read Grep Glob Edit Write Bash");
        let mpos = argv.iter().position(|a| a == "--model").unwrap();
        assert_eq!(argv[mpos + 1], "opus");
    }

    #[test]
    fn claude_argv_omits_model_when_unset() {
        let cmd = build_claude_command(
            Path::new("claude"),
            None,
            &default_allowed_tools(),
            "t",
            Path::new("/tmp/wt"),
        );
        let argv: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!argv.iter().any(|a| a == "--model"));
    }

    #[test]
    fn parse_assistant_text_and_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"text","text":"thinking about it"},
            {"type":"tool_use","name":"Edit","input":{"file_path":"src/lib.rs"}}
        ]}}"#;
        let out = parse_stream_line(line);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, AgentOutputKind::Text);
        assert_eq!(out[0].1, "thinking about it");
        assert_eq!(out[1].0, AgentOutputKind::ToolUse);
        assert!(out[1].1.starts_with("Edit "), "{}", out[1].1);
    }

    #[test]
    fn parse_tool_result_string_and_blocks() {
        let s = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"45 tests passed"}]}}"#;
        let out = parse_stream_line(s);
        assert_eq!(
            out,
            vec![(AgentOutputKind::ToolResult, "45 tests passed".into())]
        );

        let blocks = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"ok"}]}]}}"#;
        let out = parse_stream_line(blocks);
        assert_eq!(out, vec![(AgentOutputKind::ToolResult, "ok".into())]);
    }

    #[test]
    fn parse_result_line_includes_cost() {
        let line = r#"{"type":"result","subtype":"success","total_cost_usd":0.1234}"#;
        let out = parse_stream_line(line);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, AgentOutputKind::System);
        assert!(out[0].1.contains("$0.1234"), "{}", out[0].1);
    }

    #[test]
    fn parse_non_json_degrades_to_text() {
        let out = parse_stream_line("plain progress line");
        assert_eq!(
            out,
            vec![(AgentOutputKind::Text, "plain progress line".into())]
        );
        assert!(parse_stream_line("   ").is_empty());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "äöü".repeat(100);
        let t = truncate(&s, 10);
        assert_eq!(t.chars().count(), 11); // 10 + ellipsis
    }

    #[tokio::test]
    async fn stub_runner_writes_scratch_and_emits() {
        let dir = tempfile::tempdir().unwrap();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = events.clone();
        let emit: EmitFn = Arc::new(move |k, t| sink.lock().unwrap().push((k, t)));
        let backend = AgentBackend::Stub {
            edits: 2,
            sleep_ms: 1,
        };
        let code = run_agent(&backend, "task", dir.path(), emit).await.unwrap();
        assert_eq!(code, Some(0));
        let scratch = std::fs::read_to_string(dir.path().join("AATXE_UI_STUB.md")).unwrap();
        assert!(scratch.contains("stub edit 2"));
        assert!(events.lock().unwrap().len() >= 4);
    }
}
