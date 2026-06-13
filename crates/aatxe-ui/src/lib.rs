//! # aatxe-ui
//!
//! Local realtime dashboard for aatxe. `aatxe ui` serves an embedded
//! single-page frontend that can:
//!
//! 1. **Watch** — live-chart any `RunReport`/`CompareReport` that arrives
//!    via `POST /api/runs`, a `perf-vs` run in `tmp/perf-vs/`, or a saved
//!    baseline in `.aatxe/baselines/`.
//! 2. **Spawn agents** — coding agents (`claude` CLI, or an offline stub)
//!    run in isolated git worktrees; every time an agent's working tree
//!    changes, its benches re-run and the head-vs-base comparison streams
//!    to the browser as a trajectory.
//! 3. **Judge** — finished agents get an `aatxe council` review of their
//!    branch diff; tournaments rank K agents on the same task by perf
//!    verdicts + council criticals.
//!
//! Everything is an event ([`events::UiEvent`]) appended to the session's
//! `events.jsonl` and broadcast over SSE — the frontend is a reducer, and
//! past sessions replay byte-identically.

pub mod agents;
pub mod bench;
pub mod bus;
pub mod council;
pub mod events;
pub mod gemini;
pub mod runner;
pub mod server;
pub mod state;
pub mod tournament;
pub mod watch;

pub use bench::BenchSpec;
pub use gemini::GeminiAgentConfig;
pub use runner::{default_allowed_tools, AgentBackend};
pub use state::{CouncilMode, UiConfig};

use crate::bus::EventBus;
use crate::events::UiEvent;
use crate::state::AppState;
use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;

/// Blocking entrypoint for the CLI: builds the runtime and serves until
/// interrupted.
pub fn serve(cfg: UiConfig) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    rt.block_on(serve_async(cfg))
}

async fn serve_async(cfg: UiConfig) -> Result<()> {
    state::ensure_self_ignoring_dir(&cfg.repo_root.join(".aatxe"))?;
    std::fs::create_dir_all(&cfg.session_dir)
        .with_context(|| format!("creating {}", cfg.session_dir.display()))?;

    let base_sha = agents::resolve_ref(&cfg.repo_root, &cfg.base_ref)
        .await
        .with_context(|| format!("resolving base ref '{}'", cfg.base_ref))?;

    let bus = Arc::new(EventBus::new(&cfg.session_dir.join("events.jsonl"))?);
    let session_started = UiEvent::SessionStarted {
        session_id: cfg.session_id.clone(),
        repo_root: cfg.repo_root.to_string_lossy().into_owned(),
        base_ref: cfg.base_ref.clone(),
        base_sha: base_sha.clone(),
        bench_label: cfg.bench.label(),
    };

    let addr = SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let open_browser = cfg.open_browser;
    let state = Arc::new(AppState {
        cfg,
        bus: bus.clone(),
        base_sha,
        agents: std::sync::Mutex::new(Default::default()),
        tournaments: std::sync::Mutex::new(Default::default()),
        agent_counter: std::sync::atomic::AtomicU64::new(0),
        base_report: tokio::sync::OnceCell::new(),
    });
    bus.emit(session_started);

    tokio::spawn(watch::watch_loop(state.clone()));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let url = format!("http://{}/", listener.local_addr().unwrap_or(addr));
    eprintln!(
        "aatxe ui: {url} · session {} · base {} · bench {}",
        state.cfg.session_id,
        agents::short(&state.base_sha),
        state.cfg.bench.label()
    );
    if open_browser {
        open_in_browser(&url);
    }
    axum::serve(listener, server::build_router(state))
        .await
        .context("serving")?;
    Ok(())
}

/// Best-effort `open`/`xdg-open`; a failure is not worth more than a note.
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    if let Err(e) = std::process::Command::new(program)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!("aatxe ui: could not open browser ({e}); open {url} manually");
    }
}
