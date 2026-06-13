//! Agent lifecycle: spawn in an isolated worktree, poll for edits, bench
//! every iteration, council on completion.
//!
//! ```text
//! spawn ─► git worktree add -b aatxe-ui/<session>-<id> … <base-sha>
//!       ─► runner task (claude -p … | stub)
//!       ─► poll loop: dirty-hash changed? ─► bench head ─► compare vs base ─► emit
//!       ─► on exit: commit, final bench, council diff, standings, AgentExited
//! ```
//!
//! The base side is benched **once per session** (shared
//! `tokio::sync::OnceCell`), in a detached worktree at the same
//! `<worktrees>/<short-sha>` path `perf-vs` uses — so a warm perf-vs
//! cargo cache is reused for free.
//!
//! A mid-iteration bench can race the agent's next edit (the build sees a
//! half-written file and fails). That's accepted: the iteration emits
//! `IterationFailed` and the next poll retries; the post-exit bench is
//! the authoritative one.

use crate::bench::run_bench;
use crate::council;
use crate::events::{AgentOutputKind, UiEvent};
use crate::runner::{run_agent, EmitFn};
use crate::state::{AgentRecord, CouncilMode, SharedState};
use crate::tournament::compute_standings;
use aatxe_core::compare_reports;
use aatxe_core::types::RunReport;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnRequest {
    pub task: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tournament_id: Option<String>,
}

/// Run a git command in `dir`, returning trimmed stdout.
pub(crate) async fn git(args: &[&str], dir: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("spawning git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub async fn resolve_ref(repo: &Path, r#ref: &str) -> Result<String> {
    git(
        &["rev-parse", "--verify", &format!("{ref}^{{commit}}")],
        repo,
    )
    .await
}

pub(crate) fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// Fingerprint the working tree: tracked diff + status + untracked file
/// stats. Any agent edit — including appends to an untracked scratch
/// file — changes the hash; that change is what triggers an iteration.
pub async fn dirty_hash(wt: &Path) -> Result<u64> {
    let mut hasher = DefaultHasher::new();
    git(&["status", "--porcelain"], wt).await?.hash(&mut hasher);
    git(&["diff", "HEAD"], wt).await?.hash(&mut hasher);
    let untracked = git(&["ls-files", "--others", "--exclude-standard"], wt).await?;
    for rel in untracked.lines().filter(|l| !l.is_empty()) {
        rel.hash(&mut hasher);
        if let Ok(meta) = std::fs::metadata(wt.join(rel)) {
            meta.len().hash(&mut hasher);
            if let Ok(mtime) = meta.modified() {
                if let Ok(d) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    d.as_nanos().hash(&mut hasher);
                }
            }
        }
    }
    Ok(hasher.finish())
}

/// Materialize (or reuse) the shared detached worktree for the base SHA.
/// Path convention matches `perf-vs`: `<worktree-parent>/<short-sha>`.
async fn ensure_base_worktree(state: &SharedState) -> Result<PathBuf> {
    let sha = &state.base_sha;
    let wt_path = state.cfg.worktree_parent.join(short(sha));
    if wt_path.exists() {
        let head = resolve_ref(&wt_path, "HEAD").await?;
        if head != *sha {
            bail!(
                "worktree at {} points at {} but base is {}; remove it and retry",
                wt_path.display(),
                short(&head),
                short(sha)
            );
        }
        return Ok(wt_path);
    }
    std::fs::create_dir_all(&state.cfg.worktree_parent)?;
    git(
        &[
            "worktree",
            "add",
            "--detach",
            &wt_path.to_string_lossy(),
            sha,
        ],
        &state.cfg.repo_root,
    )
    .await?;
    Ok(wt_path)
}

/// Bench the base side exactly once per session; everyone awaits the cell.
async fn base_report(state: &SharedState) -> Result<&RunReport> {
    state
        .base_report
        .get_or_try_init(|| async {
            state.bus.emit(UiEvent::Notice {
                message: format!(
                    "benching base {} (once per session)…",
                    short(&state.base_sha)
                ),
            });
            let wt = ensure_base_worktree(state).await?;
            let report = run_bench(&state.cfg.bench, &wt, short(&state.base_sha)).await?;
            state.bus.emit(UiEvent::Notice {
                message: format!(
                    "base benched: {} runs from {}",
                    report.runs.len(),
                    short(&state.base_sha)
                ),
            });
            Ok::<_, anyhow::Error>(report)
        })
        .await
}

/// Create the worktree + record, emit `AgentSpawned`, and detach the
/// lifecycle task. Returns the new agent id.
pub async fn spawn_agent(state: SharedState, req: SpawnRequest) -> Result<String> {
    let active = state
        .agents
        .lock()
        .map(|a| a.values().filter(|r| !r.done).count())
        .unwrap_or(0);
    if active >= state.cfg.max_agents {
        bail!(
            "agent cap reached ({} active, max {})",
            active,
            state.cfg.max_agents
        );
    }
    if req.task.trim().is_empty() {
        bail!("task must not be empty");
    }

    let n = state.agent_counter.fetch_add(1, Ordering::SeqCst) + 1;
    let agent_id = format!("a{n}");
    let name = req.name.clone().unwrap_or_else(|| format!("agent-{n}"));
    let branch = format!("aatxe-ui/{}-{}", state.cfg.session_id, agent_id);
    let wt = state
        .cfg
        .worktree_parent
        .join("agents")
        .join(&state.cfg.session_id)
        .join(&agent_id);
    std::fs::create_dir_all(wt.parent().expect("agent worktree has parent"))?;
    git(
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &wt.to_string_lossy(),
            &state.base_sha,
        ],
        &state.cfg.repo_root,
    )
    .await
    .context("creating agent worktree")?;

    let record = AgentRecord {
        agent_id: agent_id.clone(),
        name: name.clone(),
        task: req.task.clone(),
        worktree: wt.clone(),
        branch: branch.clone(),
        tournament_id: req.tournament_id.clone(),
        iterations: 0,
        done: false,
        latest_compare: None,
        council_critical: None,
    };
    if let Ok(mut agents) = state.agents.lock() {
        agents.insert(agent_id.clone(), record);
    }
    state.bus.emit(UiEvent::AgentSpawned {
        agent_id: agent_id.clone(),
        name,
        task: req.task.clone(),
        worktree: wt.to_string_lossy().into_owned(),
        branch,
        tournament_id: req.tournament_id.clone(),
    });

    let lifecycle_state = state.clone();
    let lifecycle_id = agent_id.clone();
    tokio::spawn(async move {
        if let Err(e) = run_lifecycle(lifecycle_state.clone(), &lifecycle_id, &req.task, &wt).await
        {
            mark_done(&lifecycle_state, &lifecycle_id);
            lifecycle_state.bus.emit(UiEvent::AgentFailed {
                agent_id: lifecycle_id.clone(),
                error: format!("{e:#}"),
            });
        }
    });
    Ok(agent_id)
}

fn mark_done(state: &SharedState, agent_id: &str) {
    if let Ok(mut agents) = state.agents.lock() {
        if let Some(r) = agents.get_mut(agent_id) {
            r.done = true;
        }
    }
}

async fn run_lifecycle(state: SharedState, agent_id: &str, task: &str, wt: &Path) -> Result<()> {
    let bus = state.bus.clone();
    let emit_id = agent_id.to_string();
    let emit: EmitFn = Arc::new(move |kind: AgentOutputKind, text: String| {
        bus.emit(UiEvent::AgentOutput {
            agent_id: emit_id.clone(),
            kind,
            text,
        });
    });

    let backend = state.cfg.backend.clone();
    let runner_task = task.to_string();
    let runner_wt = wt.to_path_buf();
    let mut runner =
        tokio::spawn(async move { run_agent(&backend, &runner_task, &runner_wt, emit).await });

    let mut last_benched = dirty_hash(wt).await.unwrap_or(0);
    let mut iterations = 0u32;
    let poll = Duration::from_secs(state.cfg.poll_secs.max(1));

    let exit_code: Option<i32> = loop {
        tokio::select! {
            joined = &mut runner => {
                break joined.map_err(|e| anyhow!("runner task panicked: {e}"))??;
            }
            _ = tokio::time::sleep(poll) => {
                match dirty_hash(wt).await {
                    Ok(h) if h != last_benched => {
                        last_benched = h;
                        iterations += 1;
                        run_iteration(&state, agent_id, iterations, wt).await;
                    }
                    _ => {}
                }
            }
        }
    };

    // Finalize: stage + commit whatever the agent left, so the branch is
    // durable and the council diff is well-defined.
    let pre_commit_hash = dirty_hash(wt).await.unwrap_or(last_benched);
    git(&["add", "-A"], wt).await.ok();
    let staged = git(&["diff", "--cached", "--name-only"], wt)
        .await
        .unwrap_or_default();
    if !staged.is_empty() {
        let title: String = task.chars().take(72).collect();
        git(
            &["commit", "-m", &format!("aatxe-ui {agent_id}: {title}")],
            wt,
        )
        .await
        .context("committing agent changes")?;
    }

    // Authoritative final bench if anything changed since the last one
    // (or nothing was ever benched but the agent did produce a diff).
    let branch_diff = git(&["diff", &state.base_sha, "HEAD"], wt)
        .await
        .unwrap_or_default();
    if pre_commit_hash != last_benched || (iterations == 0 && !branch_diff.is_empty()) {
        iterations += 1;
        run_iteration(&state, agent_id, iterations, wt).await;
    }

    // Council lane.
    if state.cfg.council != CouncilMode::Off {
        if branch_diff.trim().is_empty() {
            state.bus.emit(UiEvent::Notice {
                message: format!("{agent_id}: empty diff, council skipped"),
            });
        } else {
            run_council_lane(&state, agent_id, wt, &branch_diff).await;
        }
    }

    mark_done(&state, agent_id);
    state.bus.emit(UiEvent::AgentExited {
        agent_id: agent_id.to_string(),
        exit_code,
        iterations,
    });
    update_standings(&state, agent_id);
    Ok(())
}

async fn run_iteration(state: &SharedState, agent_id: &str, iteration: u32, wt: &Path) {
    state.bus.emit(UiEvent::IterationStarted {
        agent_id: agent_id.to_string(),
        iteration,
    });
    let result: Result<()> = async {
        let base = base_report(state).await?;
        let head = run_bench(&state.cfg.bench, wt, &format!("{agent_id}#{iteration}")).await?;
        let cmp = compare_reports(base, &head, state.cfg.compare_options());
        if let Ok(mut agents) = state.agents.lock() {
            if let Some(r) = agents.get_mut(agent_id) {
                r.latest_compare = Some(cmp.clone());
                r.iterations = iteration;
            }
        }
        state.bus.emit(UiEvent::IterationCompare {
            agent_id: agent_id.to_string(),
            iteration,
            report: Box::new(cmp),
        });
        Ok(())
    }
    .await;
    if let Err(e) = result {
        state.bus.emit(UiEvent::IterationFailed {
            agent_id: agent_id.to_string(),
            iteration,
            error: format!("{e:#}"),
        });
    }
    update_standings(state, agent_id);
}

async fn run_council_lane(state: &SharedState, agent_id: &str, wt: &Path, diff: &str) {
    state.bus.emit(UiEvent::CouncilStarted {
        agent_id: agent_id.to_string(),
    });
    let out_dir = state.cfg.session_dir.join("agents").join(agent_id);
    let result: Result<()> = async {
        std::fs::create_dir_all(&out_dir)?;
        let diff_path = out_dir.join("council.diff");
        std::fs::write(&diff_path, diff)?;
        let (counts, markdown) = council::run_council(
            &state.cfg.aatxe_bin,
            state.cfg.council,
            &diff_path,
            &out_dir,
            wt,
            state.cfg.confidence_floor,
        )
        .await?;
        if let Ok(mut agents) = state.agents.lock() {
            if let Some(r) = agents.get_mut(agent_id) {
                r.council_critical = Some(counts.critical);
            }
        }
        state.bus.emit(UiEvent::CouncilVerdict {
            agent_id: agent_id.to_string(),
            critical: counts.critical,
            major: counts.major,
            shippable: counts.shippable,
            markdown,
        });
        Ok(())
    }
    .await;
    if let Err(e) = result {
        state.bus.emit(UiEvent::CouncilFailed {
            agent_id: agent_id.to_string(),
            error: format!("{e:#}"),
        });
    }
    update_standings(state, agent_id);
}

/// Recompute + emit standings for the tournament this agent belongs to.
fn update_standings(state: &SharedState, agent_id: &str) {
    let Ok(agents) = state.agents.lock() else {
        return;
    };
    let Some(tid) = agents.get(agent_id).and_then(|r| r.tournament_id.clone()) else {
        return;
    };
    let members: Vec<AgentRecord> = agents
        .values()
        .filter(|r| r.tournament_id.as_deref() == Some(tid.as_str()))
        .cloned()
        .collect();
    drop(agents);
    let standings = compute_standings(&members);
    state.bus.emit(UiEvent::TournamentStandings {
        tournament_id: tid,
        standings,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn init_repo(dir: &Path) {
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            git(&args, dir).await.unwrap();
        }
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        git(&["add", "."], dir).await.unwrap();
        git(&["commit", "-q", "-m", "init"], dir).await.unwrap();
    }

    #[tokio::test]
    async fn dirty_hash_changes_on_tracked_edit_and_untracked_append() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let clean = dirty_hash(dir.path()).await.unwrap();

        // Tracked-file edit changes the hash.
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        let tracked = dirty_hash(dir.path()).await.unwrap();
        assert_ne!(clean, tracked);

        // Untracked scratch file (the stub runner's write path) too.
        std::fs::write(dir.path().join("scratch.md"), "one\n").unwrap();
        let untracked1 = dirty_hash(dir.path()).await.unwrap();
        assert_ne!(tracked, untracked1);

        // …and growing that same untracked file again.
        std::fs::write(dir.path().join("scratch.md"), "one\ntwo\n").unwrap();
        let untracked2 = dirty_hash(dir.path()).await.unwrap();
        assert_ne!(untracked1, untracked2);
    }

    #[tokio::test]
    async fn dirty_hash_is_stable_when_nothing_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let h1 = dirty_hash(dir.path()).await.unwrap();
        let h2 = dirty_hash(dir.path()).await.unwrap();
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn resolve_ref_round_trips_head() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let sha = resolve_ref(dir.path(), "HEAD").await.unwrap();
        assert_eq!(sha.len(), 40);
        assert_eq!(short(&sha).len(), 8);
    }
}
