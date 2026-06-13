//! Server configuration + shared mutable state.

use crate::bench::BenchSpec;
use crate::bus::EventBus;
use crate::runner::AgentBackend;
use aatxe_core::types::{CompareReport, RunReport};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CouncilMode {
    Off,
    /// `AATXE_COUNCIL_STUB=1` — proves plumbing, free and offline.
    Stub,
    /// `--backend claude-code` — real review via the local `claude` CLI.
    Real,
}

pub struct UiConfig {
    pub repo_root: PathBuf,
    pub port: u16,
    pub base_ref: String,
    pub bench: BenchSpec,
    pub worktree_parent: PathBuf,
    pub session_id: String,
    pub session_dir: PathBuf,
    pub poll_secs: u64,
    pub backend: AgentBackend,
    pub council: CouncilMode,
    pub threshold: f64,
    pub alpha: f64,
    pub noisy_cv: f64,
    pub confidence_floor: f64,
    pub open_browser: bool,
    pub max_agents: usize,
    /// The `aatxe` binary used for council runs. Defaults to
    /// `current_exe()` so the dashboard drives the same build it ships in.
    pub aatxe_bin: PathBuf,
}

impl UiConfig {
    pub fn compare_options(&self) -> aatxe_core::CompareOptions {
        aatxe_core::CompareOptions {
            threshold_pct: self.threshold,
            alpha: self.alpha,
            noisy_cv_threshold: self.noisy_cv,
        }
    }
}

/// Live snapshot of one agent, kept server-side so `/api/state` can serve
/// a late-joining client without replaying the event log on the server.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRecord {
    pub agent_id: String,
    pub name: String,
    pub task: String,
    pub worktree: PathBuf,
    pub branch: String,
    pub tournament_id: Option<String>,
    pub iterations: u32,
    pub done: bool,
    #[serde(skip)]
    pub latest_compare: Option<CompareReport>,
    pub council_critical: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tournament {
    pub tournament_id: String,
    pub task: String,
    pub agent_ids: Vec<String>,
}

pub struct AppState {
    pub cfg: UiConfig,
    pub bus: Arc<EventBus>,
    pub base_sha: String,
    pub agents: Mutex<HashMap<String, AgentRecord>>,
    pub tournaments: Mutex<HashMap<String, Tournament>>,
    pub agent_counter: AtomicU64,
    /// One cell per base worktree bench so N agents share a single base
    /// measurement instead of re-benching the same SHA N times.
    pub base_report: tokio::sync::OnceCell<RunReport>,
}

pub type SharedState = Arc<AppState>;

/// Make `dir` exist and be invisible to git (a `.gitignore` containing
/// `*` inside it) — same self-gitignoring trick as `.aatxe/baselines/`.
pub fn ensure_self_ignoring_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, "*\n").with_context(|| format!("writing {}", ignore.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub session_id: String,
    pub events: u64,
    /// `ts_ms` of the first event, 0 when unreadable.
    pub started_ms: u64,
}

/// List past sessions, newest first, by scanning `.aatxe/ui/sessions/`.
pub fn list_sessions(repo_root: &Path) -> Vec<SessionMeta> {
    let root = sessions_root(repo_root);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<SessionMeta> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            let events = crate::bus::EventBus::read_jsonl(&e.path().join("events.jsonl"));
            if events.is_empty() {
                return None;
            }
            Some(SessionMeta {
                session_id: id,
                events: events.len() as u64,
                started_ms: events.first().map(|e| e.ts_ms).unwrap_or(0),
            })
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.started_ms));
    out
}

pub fn sessions_root(repo_root: &Path) -> PathBuf {
    repo_root.join(".aatxe/ui/sessions")
}

pub fn session_events_path(repo_root: &Path, session_id: &str) -> PathBuf {
    sessions_root(repo_root)
        .join(session_id)
        .join("events.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::UiEvent;

    #[test]
    fn self_ignoring_dir_writes_gitignore_once() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".aatxe/ui");
        ensure_self_ignoring_dir(&target).unwrap();
        let ignore = target.join(".gitignore");
        assert_eq!(std::fs::read_to_string(&ignore).unwrap(), "*\n");
        // Second call must not clobber a hand-edited file.
        std::fs::write(&ignore, "custom\n").unwrap();
        ensure_self_ignoring_dir(&target).unwrap();
        assert_eq!(std::fs::read_to_string(&ignore).unwrap(), "custom\n");
    }

    #[test]
    fn list_sessions_orders_newest_first_and_skips_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        for (id, ts) in [("s-old", 100u64), ("s-new", 200u64)] {
            let p = sessions_root(repo).join(id);
            std::fs::create_dir_all(&p).unwrap();
            let bus = crate::bus::EventBus::new(&p.join("events.jsonl")).unwrap();
            // Manually-shaped line so started_ms is deterministic.
            drop(bus);
            std::fs::write(
                p.join("events.jsonl"),
                format!(
                    "{}\n",
                    serde_json::to_string(&crate::events::Envelope {
                        seq: 1,
                        ts_ms: ts,
                        event: UiEvent::Notice {
                            message: "x".into()
                        },
                    })
                    .unwrap()
                ),
            )
            .unwrap();
        }
        // An empty session dir must be skipped.
        std::fs::create_dir_all(sessions_root(repo).join("s-empty")).unwrap();
        let sessions = list_sessions(repo);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "s-new");
        assert_eq!(sessions[1].session_id, "s-old");
    }
}
