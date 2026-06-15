//! Axum router: embedded static frontend + JSON API + SSE event stream.
//!
//! The frontend is a Svelte app (source in `ui/`) compiled by Vite into
//! `assets/{index.html,app.js,app.css}`, which are baked into the binary with
//! `include_str!`. The build output is committed, so `cargo install aatxe`
//! ships the whole dashboard with no Node toolchain at build time — only when
//! changing the frontend (`make ui-build`).

use crate::agents::{self, SpawnRequest};
use crate::bus::EventBus;
use crate::events::{Envelope, UiEvent};
use crate::state::{list_sessions, session_events_path, SharedState, Tournament};
use crate::tournament::STRATEGY_HINTS;
use aatxe_core::types::RunReport;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

pub fn build_router(state: SharedState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/app.css", get(app_css))
        .route("/assets/app.js", get(app_js))
        .route("/api/events", get(sse_events))
        .route("/api/state", get(api_state))
        .route("/api/runs", post(ingest_run))
        .route("/api/agents", post(post_agent))
        .route("/api/agents/:id/diff", get(agent_diff))
        .route("/api/tournaments", post(post_tournament))
        .route("/api/sessions", get(get_sessions))
        .route("/api/sessions/:id/events", get(get_session_events))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

async fn app_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css")],
        include_str!("../assets/app.css"),
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        include_str!("../assets/app.js"),
    )
}

#[derive(Debug, Deserialize)]
struct SinceQuery {
    #[serde(default)]
    since: Option<u64>,
}

/// SSE stream: catch-up replay from the session JSONL, then live events.
/// Subscribe-before-replay plus seq-based dedup means no gap and no
/// duplicates across the seam.
async fn sse_events(
    State(state): State<SharedState>,
    Query(q): Query<SinceQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let since = q.since.unwrap_or(0);
    let rx = state.bus.subscribe();
    let replay: Vec<Envelope> = state
        .bus
        .replay()
        .into_iter()
        .filter(|e| e.seq > since)
        .collect();
    let last_replayed = replay.last().map(|e| e.seq).unwrap_or(since);
    let live = BroadcastStream::new(rx)
        .filter_map(|r| r.ok())
        .filter(move |e| e.seq > last_replayed);
    let stream = tokio_stream::iter(replay)
        .chain(live)
        .map(|env| Ok(to_sse_event(&env)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn to_sse_event(env: &Envelope) -> Event {
    Event::default()
        .id(env.seq.to_string())
        .data(serde_json::to_string(env).unwrap_or_else(|_| "{}".into()))
}

async fn api_state(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let agents: Vec<_> = state
        .agents
        .lock()
        .map(|a| a.values().cloned().collect())
        .unwrap_or_default();
    let tournaments: Vec<Tournament> = state
        .tournaments
        .lock()
        .map(|t| t.values().cloned().collect())
        .unwrap_or_default();
    Json(serde_json::json!({
        "sessionId": state.cfg.session_id,
        "repoRoot": state.cfg.repo_root,
        "baseRef": state.cfg.base_ref,
        "baseSha": state.base_sha,
        "benchLabel": state.cfg.bench.label(),
        "agents": agents,
        "tournaments": tournaments,
    }))
}

async fn ingest_run(State(state): State<SharedState>, Json(report): Json<RunReport>) -> StatusCode {
    state.bus.emit(UiEvent::RunIngested {
        source: "api".into(),
        report: Box::new(report),
    });
    StatusCode::ACCEPTED
}

async fn post_agent(
    State(state): State<SharedState>,
    Json(req): Json<SpawnRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let agent_id = agents::spawn_agent(state, req)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(Json(serde_json::json!({ "agentId": agent_id })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TournamentRequest {
    task: String,
    count: usize,
}

async fn post_tournament(
    State(state): State<SharedState>,
    Json(req): Json<TournamentRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.task.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "task must not be empty".into()));
    }
    let count = req.count.clamp(2, STRATEGY_HINTS.len());
    let tournament_id = format!(
        "t{}",
        state.tournaments.lock().map(|t| t.len() + 1).unwrap_or(1)
    );
    let mut agent_ids = Vec::with_capacity(count);
    for hint in STRATEGY_HINTS.iter().take(count) {
        let name = hint.split(':').next().unwrap_or("agent").to_string();
        let task = format!("{}\n\nStrategy emphasis: {hint}.", req.task);
        let id = agents::spawn_agent(
            state.clone(),
            SpawnRequest {
                task,
                name: Some(name),
                tournament_id: Some(tournament_id.clone()),
            },
        )
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
        agent_ids.push(id);
    }
    if let Ok(mut t) = state.tournaments.lock() {
        t.insert(
            tournament_id.clone(),
            Tournament {
                tournament_id: tournament_id.clone(),
                task: req.task.clone(),
                agent_ids: agent_ids.clone(),
            },
        );
    }
    state.bus.emit(UiEvent::TournamentStarted {
        tournament_id: tournament_id.clone(),
        task: req.task,
        agent_ids: agent_ids.clone(),
    });
    Ok(Json(serde_json::json!({
        "tournamentId": tournament_id,
        "agentIds": agent_ids,
    })))
}

async fn agent_diff(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
) -> Result<String, (StatusCode, String)> {
    let worktree = state
        .agents
        .lock()
        .ok()
        .and_then(|a| a.get(&id).map(|r| r.worktree.clone()))
        .ok_or((StatusCode::NOT_FOUND, format!("no agent {id}")))?;
    agents::git(&["diff", &state.base_sha], &worktree)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

async fn get_sessions(State(state): State<SharedState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(list_sessions(&state.cfg.repo_root)))
}

async fn get_session_events(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<Envelope>>, (StatusCode, String)> {
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err((StatusCode::BAD_REQUEST, "invalid session id".into()));
    }
    let path = session_events_path(&state.cfg.repo_root, &id);
    if !path.is_file() {
        return Err((StatusCode::NOT_FOUND, format!("no session {id}")));
    }
    Ok(Json(EventBus::read_jsonl(&path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::BenchSpec;
    use crate::runner::AgentBackend;
    use crate::state::{AppState, CouncilMode, UiConfig};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::path::Path;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_state(repo_root: &Path) -> SharedState {
        let session_dir = repo_root.join(".aatxe/ui/sessions/s-test");
        std::fs::create_dir_all(&session_dir).unwrap();
        let bus = Arc::new(EventBus::new(&session_dir.join("events.jsonl")).unwrap());
        Arc::new(AppState {
            cfg: UiConfig {
                repo_root: repo_root.to_path_buf(),
                port: 0,
                base_ref: "HEAD".into(),
                bench: BenchSpec::Command("true".into()),
                worktree_parent: repo_root.join("wt"),
                session_id: "s-test".into(),
                session_dir,
                poll_secs: 1,
                backend: AgentBackend::Stub {
                    edits: 1,
                    sleep_ms: 1,
                },
                council: CouncilMode::Off,
                threshold: 0.05,
                alpha: 0.05,
                noisy_cv: 0.25,
                confidence_floor: 0.55,
                open_browser: false,
                max_agents: 4,
                aatxe_bin: "aatxe".into(),
            },
            bus,
            base_sha: "0000000000000000000000000000000000000000".into(),
            agents: std::sync::Mutex::new(Default::default()),
            tournaments: std::sync::Mutex::new(Default::default()),
            agent_counter: std::sync::atomic::AtomicU64::new(0),
            base_report: tokio::sync::OnceCell::new(),
        })
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn ingest_run_emits_event_and_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let app = build_router(state.clone());
        let report = crate::bench::test_support::sample_report("x", 10.0);
        let resp = app
            .oneshot(
                Request::post("/api/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&report).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let replay = state.bus.replay();
        assert_eq!(replay.len(), 1);
        assert!(matches!(replay[0].event, UiEvent::RunIngested { .. }));
    }

    #[tokio::test]
    async fn empty_task_is_rejected_with_400() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post("/api/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"task":"   "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn api_state_reports_session_and_bench() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let app = build_router(state);
        let resp = app
            .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["sessionId"], "s-test");
        assert!(v["benchLabel"].as_str().unwrap().starts_with("cmd:"));
    }

    #[tokio::test]
    async fn session_events_validates_id_and_replays() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        state.bus.emit(UiEvent::Notice {
            message: "hi".into(),
        });
        let app = build_router(state.clone());

        let bad = app
            .clone()
            .oneshot(
                Request::get("/api/sessions/..%2Fetc/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

        let ok = app
            .oneshot(
                Request::get("/api/sessions/s-test/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let v = body_json(ok).await;
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn index_and_assets_are_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let app = build_router(test_state(dir.path()));
        for path in ["/", "/assets/app.js", "/assets/app.css"] {
            let resp = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
        }
    }

    #[tokio::test]
    async fn unknown_agent_diff_is_404() {
        let dir = tempfile::tempdir().unwrap();
        let app = build_router(test_state(dir.path()));
        let resp = app
            .oneshot(
                Request::get("/api/agents/nope/diff")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
