//! Shared helpers for [`LlmClient`] backends that drive a child CLI.
//!
//! Both [`crate::llm::pi_proxy`] and [`crate::llm::claude_code`] follow the same
//! shape: build an argv, spawn the binary, stream the user payload on
//! stdin, wait with a wall-clock deadline, then sanitise the model's
//! stdout. The pieces shared between the two live here so a bug fix or
//! behavioural tweak (e.g. fence-stripping rules) only has to happen
//! once.

use aatxe_council::llm::{ChatMessage, LlmError, Role};
use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Split chat messages into system content vs. everything else,
/// preserving order within each bucket. The council never sends
/// assistant turns today; if it ever does they'll be folded into the
/// user blob alongside user turns.
pub(crate) fn partition_messages(messages: &[ChatMessage]) -> (Vec<&String>, Vec<&String>) {
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

/// Concatenate string parts with blank-line separators.
pub(crate) fn join_with_blank_lines(parts: &[&String]) -> String {
    let mut acc = String::new();
    for p in parts {
        if !acc.is_empty() {
            acc.push_str("\n\n");
        }
        acc.push_str(p);
    }
    acc
}

/// Strip a single matching ```\<lang>\n...\n``` markdown fence from
/// `raw`. Some agent CLIs occasionally wrap JSON answers in a fence
/// even after being told not to; the proposer/judge parsers tolerate
/// prose around the JSON object, but stripping a fence here is cheap
/// insurance and keeps every backend's behaviour identical.
///
/// Trims leading/trailing whitespace so the parser sees a clean
/// `{` … `}` boundary.
pub(crate) fn sanitize_text_output(raw: &str) -> String {
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

/// Spawn `cmd` with piped stdio, stream `stdin_blob` in, then wait up
/// to `timeout` for it to exit. On overrun the child is killed and a
/// `Transport` error is returned naming `binary_label` (e.g. `"pi"`,
/// `"claude"`).
///
/// `t0` is the spawn-relative deadline anchor — callers usually pass
/// `Instant::now()` from the start of their `chat` method so the
/// timeout budget includes spawn cost.
pub(crate) fn spawn_and_wait(
    mut cmd: Command,
    stdin_blob: &str,
    timeout: Duration,
    t0: Instant,
    binary_label: &str,
) -> Result<Output, LlmError> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| LlmError::Transport(format!("spawning {binary_label}: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_blob.as_bytes())
            .map_err(|e| LlmError::Transport(format!("writing stdin to {binary_label}: {e}")))?;
    }

    // Bounded wait. `Child::wait_with_output` doesn't take a timeout
    // on stable Rust, so we poll `try_wait` and `kill` on overrun.
    let deadline = t0 + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                return child.wait_with_output().map_err(|e| {
                    LlmError::Transport(format!("collecting {binary_label} output: {e}"))
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(LlmError::Transport(format!(
                        "{binary_label} did not exit within {} seconds",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(LlmError::Transport(format!(
                    "waiting on {binary_label}: {e}"
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Test fixtures shared between pi_proxy::tests and claude_code::tests.
// Both install a `#!/bin/sh` shell script as a fake binary and chmod it
// via `std::os::unix::fs::PermissionsExt`, so the harness is Unix-only.
#[cfg(all(test, unix))]
pub(crate) mod test_fixture {
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Compose a tiny shell-script binary that mimics enough of the
    /// real agent CLI for unit tests. The script:
    ///   * echoes `canned_stdout` to stdout
    ///   * exits with `exit_code`
    ///   * writes the invocation argv + stdin to a sidecar file so the
    ///     test can assert what the client actually called.
    ///
    /// Returns the path of the fake binary + the path of the capture
    /// file. Caller owns both via the returned [`TempDir`].
    pub(crate) fn fake_binary(
        name: &str,
        canned_stdout: &str,
        exit_code: i32,
    ) -> (TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join(name);
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
        fs::write(&bin, script).expect("write fake binary");
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
        (dir, bin, capture)
    }

    /// Install a sleep-only fake binary at `<dir>/<name>`. The caller
    /// gets back the binary path; the [`TempDir`] keeps the file alive.
    pub(crate) fn fake_sleeping_binary(name: &str, seconds: u32) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join(name);
        fs::write(&bin, format!("#!/bin/sh\nsleep {seconds}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut p = fs::metadata(&bin).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(&bin, p).unwrap();
        (dir, bin)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use aatxe_council::llm::ChatMessage;

    #[test]
    fn partition_messages_routes_system_to_left_and_others_to_right() {
        let msgs = vec![
            ChatMessage::system("s1"),
            ChatMessage::user("u1"),
            ChatMessage::system("s2"),
            ChatMessage::user("u2"),
        ];
        let (sys, user) = partition_messages(&msgs);
        assert_eq!(
            sys.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            vec!["s1", "s2"]
        );
        assert_eq!(
            user.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            vec!["u1", "u2"]
        );
    }

    #[test]
    fn join_with_blank_lines_separates_with_double_newline() {
        let a = "alpha".to_string();
        let b = "beta".to_string();
        let parts: Vec<&String> = vec![&a, &b];
        assert_eq!(join_with_blank_lines(&parts), "alpha\n\nbeta");
        assert_eq!(join_with_blank_lines(&[]), "");
    }

    #[test]
    fn sanitize_strips_only_fenced_blocks() {
        assert_eq!(sanitize_text_output("plain"), "plain");
        assert_eq!(sanitize_text_output("   plain   "), "plain");
        assert_eq!(
            sanitize_text_output("```json\n{\"a\":1}\n```"),
            r#"{"a":1}"#
        );
        // Unterminated fence — leave as-is.
        assert_eq!(
            sanitize_text_output("```json\n{\"a\":1}\n").trim_end(),
            "```json\n{\"a\":1}"
        );
    }
}
