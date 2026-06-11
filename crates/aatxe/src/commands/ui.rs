//! `aatxe ui` — map CLI args onto [`aatxe_ui::UiConfig`] and serve.
//!
//! Business logic lives in the `aatxe-ui` crate; this module only
//! resolves defaults that need the CLI's context (repo root discovery,
//! bench-arg → bench-spec mapping, session id, current exe).

use crate::cli::{PerfBenchArg, UiAgentArg, UiArgs, UiCouncilArg};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn execute(args: UiArgs) -> Result<()> {
    let repo_root = match &args.repo {
        Some(p) => repo_root_of(p)?,
        None => repo_root_of(Path::new("."))?,
    };

    let bench = match &args.bench_cmd {
        Some(cmd) => aatxe_ui::BenchSpec::Command(cmd.clone()),
        None => aatxe_ui::BenchSpec::CargoBins(
            bench_bins(args.bench)
                .into_iter()
                .map(String::from)
                .collect(),
        ),
    };

    let backend = match args.agent_backend {
        UiAgentArg::Stub => aatxe_ui::AgentBackend::Stub {
            edits: 3,
            sleep_ms: 4_000,
        },
        UiAgentArg::Claude => aatxe_ui::AgentBackend::ClaudeCode {
            binary: args
                .claude_binary
                .clone()
                .unwrap_or_else(|| PathBuf::from("claude")),
            model: args.model.clone(),
            allowed_tools: aatxe_ui::default_allowed_tools(),
        },
        UiAgentArg::Gemini => {
            let api_key = std::env::var("GEMINI_API_KEY")
                .ok()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "GEMINI_API_KEY is not set. Export it (or point GEMINI_ENV at an \
                         env file and use `make ui`, which sources it for you)."
                    )
                })?;
            aatxe_ui::AgentBackend::Gemini(aatxe_ui::GeminiAgentConfig::new(
                api_key,
                args.gemini_model.clone(),
                std::env::var("GEMINI_BASE_URL")
                    .ok()
                    .filter(|s| !s.is_empty()),
            ))
        }
    };

    let council = match args.council {
        UiCouncilArg::Off => aatxe_ui::CouncilMode::Off,
        UiCouncilArg::Stub => aatxe_ui::CouncilMode::Stub,
        UiCouncilArg::Real => aatxe_ui::CouncilMode::Real,
    };

    let session_id = format!(
        "s{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    let session_dir = repo_root.join(".aatxe/ui/sessions").join(&session_id);
    let worktree_parent = args.worktree_dir.clone().unwrap_or_else(|| {
        repo_root
            .parent()
            .map(|p| p.join("aatxe-worktrees"))
            .unwrap_or_else(|| repo_root.join(".aatxe-worktrees"))
    });

    aatxe_ui::serve(aatxe_ui::UiConfig {
        repo_root,
        port: args.port,
        base_ref: args.base.clone(),
        bench,
        worktree_parent,
        session_id,
        session_dir,
        poll_secs: args.poll_secs,
        backend,
        council,
        threshold: args.threshold,
        alpha: args.alpha,
        noisy_cv: args.noisy_cv,
        confidence_floor: args.confidence_floor,
        open_browser: !args.no_open,
        max_agents: args.max_agents,
        aatxe_bin: std::env::current_exe().context("resolving current exe")?,
    })
}

fn bench_bins(b: PerfBenchArg) -> Vec<&'static str> {
    match b {
        PerfBenchArg::Council => vec!["aatxe-council-bench"],
        PerfBenchArg::BigDiff => vec!["aatxe-big-diff-bench"],
        PerfBenchArg::Core => vec!["aatxe-core-bench"],
        PerfBenchArg::Ast => vec!["aatxe-ast-bench"],
        PerfBenchArg::All => vec![
            "aatxe-council-bench",
            "aatxe-big-diff-bench",
            "aatxe-core-bench",
            "aatxe-ast-bench",
        ],
    }
}

fn repo_root_of(dir: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .context("running `git rev-parse --show-toplevel`")?;
    if !out.status.success() {
        bail!(
            "{} is not inside a git repo: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(PathBuf::from(
        String::from_utf8(out.stdout)?.trim().to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_bins_cover_all_variants() {
        assert_eq!(
            bench_bins(PerfBenchArg::Council),
            vec!["aatxe-council-bench"]
        );
        assert_eq!(
            bench_bins(PerfBenchArg::BigDiff),
            vec!["aatxe-big-diff-bench"]
        );
        assert_eq!(bench_bins(PerfBenchArg::Core), vec!["aatxe-core-bench"]);
        assert_eq!(bench_bins(PerfBenchArg::Ast), vec!["aatxe-ast-bench"]);
        // `All` must stay in lockstep with every concrete variant.
        assert_eq!(bench_bins(PerfBenchArg::All).len(), 4);
    }

    #[test]
    fn repo_root_errors_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert!(repo_root_of(dir.path()).is_err());
    }
}
