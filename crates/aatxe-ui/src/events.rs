//! Event model for the dashboard stream.
//!
//! Every fact the UI knows arrives as a [`UiEvent`] wrapped in an
//! [`Envelope`]: produced once, appended to the session's `events.jsonl`,
//! and broadcast to every connected SSE client. The frontend is a pure
//! reducer over this stream — reconnecting (or opening a past session)
//! replays the JSONL and arrives at the same screen.

use aatxe_core::{CompareReport, RunReport};
use serde::{Deserialize, Serialize};

/// Wire envelope around every [`UiEvent`]. `seq` is monotonically
/// increasing within a session so clients can resume with `?since=`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub seq: u64,
    /// Unix epoch milliseconds at emit time.
    pub ts_ms: u64,
    #[serde(flatten)]
    pub event: UiEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UiEvent {
    /// First event of every session.
    SessionStarted {
        session_id: String,
        repo_root: String,
        base_ref: String,
        base_sha: String,
        bench_label: String,
    },
    /// A `RunReport` arrived from outside the agent loop — `POST
    /// /api/runs` or a watched baseline file.
    RunIngested {
        source: String,
        report: Box<RunReport>,
    },
    /// A `CompareReport` produced outside the agent loop — e.g. a
    /// `perf-vs` run that landed in `tmp/perf-vs/` while the UI was up.
    ExternalCompare {
        source: String,
        report: Box<CompareReport>,
    },
    AgentSpawned {
        agent_id: String,
        name: String,
        task: String,
        worktree: String,
        branch: String,
        tournament_id: Option<String>,
    },
    /// One line of agent activity (assistant text, tool call, stderr…).
    AgentOutput {
        agent_id: String,
        kind: AgentOutputKind,
        text: String,
    },
    IterationStarted {
        agent_id: String,
        iteration: u32,
    },
    /// The load-bearing event: head-vs-base comparison for one iteration
    /// of one agent. The frontend appends a point to the trajectory.
    IterationCompare {
        agent_id: String,
        iteration: u32,
        report: Box<CompareReport>,
    },
    IterationFailed {
        agent_id: String,
        iteration: u32,
        error: String,
    },
    CouncilStarted {
        agent_id: String,
    },
    CouncilVerdict {
        agent_id: String,
        critical: u32,
        major: u32,
        shippable: u32,
        markdown: String,
    },
    CouncilFailed {
        agent_id: String,
        error: String,
    },
    AgentExited {
        agent_id: String,
        exit_code: Option<i32>,
        iterations: u32,
    },
    AgentFailed {
        agent_id: String,
        error: String,
    },
    TournamentStarted {
        tournament_id: String,
        task: String,
        agent_ids: Vec<String>,
    },
    TournamentStandings {
        tournament_id: String,
        standings: Vec<Standing>,
    },
    /// Free-form operator-facing note (base bench cached, empty diff…).
    Notice {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentOutputKind {
    Text,
    ToolUse,
    ToolResult,
    System,
    Stderr,
}

/// One row of a tournament leaderboard. See `tournament::compute_standings`
/// for the scoring rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Standing {
    pub agent_id: String,
    pub name: String,
    pub rank: u32,
    pub score: f64,
    pub regressions: u32,
    pub improvements: u32,
    /// `None` until the agent's council lane has reported.
    pub council_critical: Option<u32>,
    /// Sum of per-bench median deltas (fraction). Negative = net faster.
    pub median_delta_sum: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_with_flattened_event() {
        let env = Envelope {
            seq: 7,
            ts_ms: 1_700_000_000_000,
            event: UiEvent::Notice {
                message: "hello".into(),
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""type":"notice""#), "{json}");
        assert!(json.contains(r#""seq":7"#), "{json}");
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.seq, 7);
        match back.event {
            UiEvent::Notice { message } => assert_eq!(message, "hello"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn event_fields_serialize_camel_case() {
        let ev = UiEvent::IterationStarted {
            agent_id: "a1".into(),
            iteration: 3,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""agentId":"a1""#), "{json}");
        assert!(json.contains(r#""type":"iterationStarted""#), "{json}");
    }

    #[test]
    fn output_kind_is_kebab_case() {
        let json = serde_json::to_string(&AgentOutputKind::ToolUse).unwrap();
        assert_eq!(json, r#""tool-use""#);
    }
}
