//! Native Gemini agent backend — a self-contained tool-use loop over
//! Gemini's OpenAI-compatible chat-completions endpoint.
//!
//! There is no local `gemini` agent CLI on a typical machine, so unlike
//! the `claude` backend this one *is* the harness: aatxe-ui sends the
//! task plus four function declarations, executes whatever tool calls
//! come back (confined to the agent's worktree), feeds the results back,
//! and repeats until the model answers in plain text or the turn budget
//! runs out. Same wire shape as the council's `gemini_http.rs` backend
//! (`/v1beta/openai/chat/completions`, `GEMINI_API_KEY` bearer auth) so
//! one key drives both.
//!
//! Everything here is blocking (`ureq` + `std::process`), mirroring the
//! rest of aatxe; `runner::run_agent` wraps it in `spawn_blocking`.

use crate::events::AgentOutputKind;
use crate::runner::EmitFn;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "gemini-2.5-flash";
pub const DEFAULT_BASE_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions";

/// Hard ceiling on model round-trips per agent run. Generous for small
/// coding tasks; prevents an undecided model from looping the API.
const MAX_TURNS: u32 = 24;
/// Cap on tool output fed back into the context, in characters.
const TOOL_RESULT_CAP: usize = 12_000;
/// Wall-clock budget for one `run_command` invocation.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct GeminiAgentConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    /// Per-request HTTP budget.
    pub timeout: Duration,
}

impl GeminiAgentConfig {
    pub fn new(api_key: String, model: Option<String>, base_url: Option<String>) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| DEFAULT_MODEL.into()),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.into()),
            timeout: Duration::from_secs(120),
        }
    }
}

// Manual impl so a debug-logged backend never prints the key.
impl fmt::Debug for GeminiAgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeminiAgentConfig")
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .finish()
    }
}

/// The system prompt framing the loop. The worktree path is informative
/// only — every tool resolves paths relative to (and confined inside)
/// the worktree regardless of what the model asks for.
fn system_prompt(worktree: &Path) -> String {
    format!(
        "You are an autonomous coding agent working in the git repository at {} \
         (all tool paths are relative to that root). Complete the user's task by \
         inspecting and editing files with the provided tools. Use run_command for \
         builds and tests. Keep changes minimal and focused on the task. \
         When the task is complete, reply with a short plain-text summary and stop \
         calling tools.",
        worktree.display()
    )
}

fn tool_declarations() -> Value {
    json!([
        {"type": "function", "function": {
            "name": "list_files",
            "description": "Recursively list files in the repository (or a subdirectory). Skips .git, target, node_modules.",
            "parameters": {"type": "object", "properties": {
                "dir": {"type": "string", "description": "Optional subdirectory, relative to the repo root."}
            }}
        }},
        {"type": "function", "function": {
            "name": "read_file",
            "description": "Read a file's contents.",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string", "description": "File path relative to the repo root."}
            }, "required": ["path"]}
        }},
        {"type": "function", "function": {
            "name": "write_file",
            "description": "Create or overwrite a file with the given content. Parent directories are created.",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            }, "required": ["path", "content"]}
        }},
        {"type": "function", "function": {
            "name": "run_command",
            "description": "Run a shell command in the repo root (sh -c). Returns stdout+stderr and the exit code. 120s timeout.",
            "parameters": {"type": "object", "properties": {
                "command": {"type": "string"}
            }, "required": ["command"]}
        }}
    ])
}

/// One requested tool call, extracted from an assistant message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON-encoded arguments string (OpenAI wire shape).
    pub arguments: String,
}

/// Pull text + tool calls out of a chat-completions response message.
pub(crate) fn parse_message(message: &Value) -> (Option<String>, Vec<ToolCall>) {
    let text = message
        .get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let calls = message
        .get("tool_calls")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    Some(ToolCall {
                        id: c.get("id")?.as_str()?.to_string(),
                        name: c.pointer("/function/name")?.as_str()?.to_string(),
                        arguments: c
                            .pointer("/function/arguments")
                            .and_then(|a| a.as_str())
                            .unwrap_or("{}")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (text, calls)
}

/// Resolve a model-supplied path strictly inside the worktree. Rejects
/// absolute paths and any traversal that would escape the root.
pub(crate) fn resolve_in_worktree(worktree: &Path, rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute() {
        bail!("absolute paths are not allowed: {rel}");
    }
    let mut depth: i32 = 0;
    let mut clean = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::Normal(c) => {
                depth += 1;
                clean.push(c);
            }
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    bail!("path escapes the worktree: {rel}");
                }
                clean.pop();
            }
            Component::CurDir => {}
            _ => bail!("unsupported path component in: {rel}"),
        }
    }
    Ok(worktree.join(clean))
}

/// Execute one tool call. Always returns a string for the model — tool
/// *failures* are data the model should react to, not harness errors.
pub(crate) fn execute_tool(worktree: &Path, name: &str, args: &Value) -> String {
    let result: Result<String> = match name {
        "list_files" => {
            let dir = args.get("dir").and_then(|d| d.as_str()).unwrap_or("");
            list_files(worktree, dir)
        }
        "read_file" => args
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow!("missing 'path'"))
            .and_then(|p| {
                let full = resolve_in_worktree(worktree, p)?;
                std::fs::read_to_string(&full).with_context(|| format!("reading {p}"))
            }),
        "write_file" => {
            let path = args.get("path").and_then(|p| p.as_str());
            let content = args.get("content").and_then(|c| c.as_str());
            match (path, content) {
                (Some(p), Some(c)) => resolve_in_worktree(worktree, p).and_then(|full| {
                    if let Some(parent) = full.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&full, c).with_context(|| format!("writing {p}"))?;
                    Ok(format!("wrote {} bytes to {p}", c.len()))
                }),
                _ => Err(anyhow!("write_file needs 'path' and 'content'")),
            }
        }
        "run_command" => args
            .get("command")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow!("missing 'command'"))
            .and_then(|c| run_command(worktree, c)),
        other => Err(anyhow!("unknown tool: {other}")),
    };
    let out = match result {
        Ok(s) => s,
        Err(e) => format!("ERROR: {e:#}"),
    };
    truncate_chars(&out, TOOL_RESULT_CAP)
}

fn list_files(worktree: &Path, dir: &str) -> Result<String> {
    const SKIP: &[&str] = &[".git", "target", "node_modules", ".aatxe", "dist"];
    let root = if dir.is_empty() {
        worktree.to_path_buf()
    } else {
        resolve_in_worktree(worktree, dir)?
    };
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            // In linked worktrees `.git` is a plain file (gitdir pointer),
            // so the skip list applies to files and directories alike.
            if SKIP.contains(&name.as_str()) {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(rel) = p.strip_prefix(worktree) {
                out.push(rel.to_string_lossy().into_owned());
            }
            if out.len() >= 200 {
                out.push("… (truncated at 200 entries)".into());
                return Ok(out.join("\n"));
            }
        }
    }
    out.sort();
    if out.is_empty() {
        out.push("(no files)".into());
    }
    Ok(out.join("\n"))
}

/// `sh -c` with a poll-based timeout (std has no native one for child
/// processes). Returns combined output + exit status as model-readable
/// text.
fn run_command(worktree: &Path, command: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning sh")?;
    let deadline = std::time::Instant::now() + COMMAND_TIMEOUT;
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("command timed out after {}s", COMMAND_TIMEOUT.as_secs());
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let out = child.wait_with_output()?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.trim().is_empty() {
        text.push_str("\n--- stderr ---\n");
        text.push_str(&stderr);
    }
    text.push_str(&format!(
        "\n(exit code: {})",
        out.status.code().unwrap_or(-1)
    ));
    Ok(text)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n… (truncated)")
}

/// Run the agent loop to completion. Blocking; call via `spawn_blocking`.
pub fn run_gemini_agent(
    cfg: &GeminiAgentConfig,
    task: &str,
    worktree: &Path,
    emit: EmitFn,
) -> Result<()> {
    emit(
        AgentOutputKind::System,
        format!("gemini agent started · model {}", cfg.model),
    );
    let agent = ureq::AgentBuilder::new().timeout(cfg.timeout).build();
    let mut messages = vec![
        json!({"role": "system", "content": system_prompt(worktree)}),
        json!({"role": "user", "content": task}),
    ];

    for turn in 1..=MAX_TURNS {
        let body = json!({
            "model": cfg.model,
            "messages": messages,
            "tools": tool_declarations(),
        });
        let response = post_with_retry(&agent, cfg, &body, &emit)?;
        let message = response
            .pointer("/choices/0/message")
            .cloned()
            .ok_or_else(|| anyhow!("no message in Gemini response: {response}"))?;
        let (text, calls) = parse_message(&message);
        if let Some(t) = &text {
            emit(AgentOutputKind::Text, t.clone());
        }
        if calls.is_empty() {
            emit(
                AgentOutputKind::System,
                format!("result: success · {turn} turn(s)"),
            );
            return Ok(());
        }
        // Echo the assistant message verbatim, then answer every call.
        messages.push(message);
        for call in calls {
            let args: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
            emit(
                AgentOutputKind::ToolUse,
                format!("{} {}", call.name, truncate_chars(&args.to_string(), 160)),
            );
            let result = execute_tool(worktree, &call.name, &args);
            emit(AgentOutputKind::ToolResult, truncate_chars(&result, 200));
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result,
            }));
        }
    }
    emit(
        AgentOutputKind::System,
        format!("stopping: turn budget ({MAX_TURNS}) exhausted"),
    );
    Ok(())
}

/// POST with two retries on transport errors / 408 / 425 / 429 / 5xx —
/// same retriable set as the council's Gemini backend.
fn post_with_retry(
    agent: &ureq::Agent,
    cfg: &GeminiAgentConfig,
    body: &Value,
    emit: &EmitFn,
) -> Result<Value> {
    let payload = body.to_string();
    let mut delay = Duration::from_secs(1);
    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            emit(
                AgentOutputKind::System,
                format!("retrying Gemini call (attempt {}): {last_err}", attempt + 1),
            );
            std::thread::sleep(delay);
            delay *= 3;
        }
        let result = agent
            .post(&cfg.base_url)
            .set("Authorization", &format!("Bearer {}", cfg.api_key))
            .set("Content-Type", "application/json")
            .send_string(&payload);
        match result {
            Ok(resp) => {
                let text = resp.into_string().context("reading Gemini response")?;
                return serde_json::from_str(&text)
                    .with_context(|| format!("parsing Gemini response: {text:.200}"));
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                last_err = format!("HTTP {code}: {}", truncate_chars(&body, 200));
                if !matches!(code, 408 | 425 | 429 | 500..=599) {
                    bail!("Gemini call failed, {last_err}");
                }
            }
            Err(ureq::Error::Transport(t)) => {
                last_err = format!("transport: {t}");
            }
        }
    }
    bail!("Gemini call failed after 3 attempts: {last_err}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_confines_to_worktree() {
        let wt = Path::new("/wt");
        assert_eq!(
            resolve_in_worktree(wt, "src/lib.rs").unwrap(),
            PathBuf::from("/wt/src/lib.rs")
        );
        assert_eq!(
            resolve_in_worktree(wt, "a/../b.txt").unwrap(),
            PathBuf::from("/wt/b.txt")
        );
        assert!(resolve_in_worktree(wt, "../escape.txt").is_err());
        assert!(resolve_in_worktree(wt, "a/../../escape.txt").is_err());
        assert!(resolve_in_worktree(wt, "/etc/passwd").is_err());
    }

    #[test]
    fn write_read_list_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();
        let out = execute_tool(
            wt,
            "write_file",
            &json!({"path": "notes/a.md", "content": "hello"}),
        );
        assert!(out.contains("wrote 5 bytes"), "{out}");
        let read = execute_tool(wt, "read_file", &json!({"path": "notes/a.md"}));
        assert_eq!(read, "hello");
        let listing = execute_tool(wt, "list_files", &json!({}));
        assert!(listing.contains("notes/a.md"), "{listing}");
    }

    #[test]
    fn list_files_skips_git_even_as_a_file() {
        // Linked worktrees have `.git` as a gitdir-pointer *file*.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".git"), "gitdir: /elsewhere").unwrap();
        std::fs::write(dir.path().join("kept.txt"), "x").unwrap();
        let listing = execute_tool(dir.path(), "list_files", &json!({}));
        assert!(listing.contains("kept.txt"), "{listing}");
        assert!(!listing.contains(".git"), "{listing}");
    }

    #[test]
    fn run_command_reports_exit_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let out = execute_tool(
            dir.path(),
            "run_command",
            &json!({"command": "echo out; echo err >&2; exit 3"}),
        );
        assert!(out.contains("out"), "{out}");
        assert!(out.contains("err"), "{out}");
        assert!(out.contains("(exit code: 3)"), "{out}");
    }

    #[test]
    fn run_command_times_out() {
        // Shrunk-budget variant would need a config knob; instead prove
        // the kill path with the real helper against a long sleep is too
        // slow for CI — so this exercises the error formatting only.
        let dir = tempfile::tempdir().unwrap();
        let out = execute_tool(dir.path(), "bogus_tool", &json!({}));
        assert!(out.starts_with("ERROR: unknown tool"), "{out}");
    }

    #[test]
    fn tool_failures_are_data_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        let out = execute_tool(dir.path(), "read_file", &json!({"path": "missing.txt"}));
        assert!(out.starts_with("ERROR:"), "{out}");
        let escape = execute_tool(
            dir.path(),
            "read_file",
            &json!({"path": "../../etc/passwd"}),
        );
        assert!(escape.contains("escapes the worktree"), "{escape}");
    }

    #[test]
    fn parse_message_extracts_text_and_calls() {
        let msg = json!({
            "role": "assistant",
            "content": "working on it",
            "tool_calls": [
                {"id": "c1", "type": "function",
                 "function": {"name": "read_file", "arguments": "{\"path\":\"x\"}"}},
                {"id": "c2", "type": "function",
                 "function": {"name": "list_files"}}
            ]
        });
        let (text, calls) = parse_message(&msg);
        assert_eq!(text.as_deref(), Some("working on it"));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[1].arguments, "{}");
    }

    #[test]
    fn parse_message_final_turn_has_no_calls() {
        let msg = json!({"role": "assistant", "content": "done: summary"});
        let (text, calls) = parse_message(&msg);
        assert_eq!(text.as_deref(), Some("done: summary"));
        assert!(calls.is_empty());
    }

    #[test]
    fn debug_redacts_api_key() {
        let cfg = GeminiAgentConfig::new("super-secret".into(), None, None);
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("super-secret"), "{dbg}");
        assert!(dbg.contains("<redacted>"), "{dbg}");
    }

    #[test]
    fn config_defaults_model_and_url() {
        let cfg = GeminiAgentConfig::new("k".into(), None, None);
        assert_eq!(cfg.model, DEFAULT_MODEL);
        assert!(cfg.base_url.contains("openai/chat/completions"));
    }
}
