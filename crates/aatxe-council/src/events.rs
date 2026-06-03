//! Pipeline-event stream. Used by the CLI's `--json-events` flag and by
//! the interactive curator (which subscribes to the same events to drive
//! its TUI). Lives in the pure crate so any consumer of the pipeline can
//! observe progress without re-implementing the orchestration.
//!
//! The default sink is [`NullSink`], which drops every event silently —
//! every existing test and CLI invocation runs through this code path
//! unchanged. Production opts in by constructing a real sink and
//! attaching it to [`crate::pipeline::CouncilOptions::event_sink`].
//!
//! ## Event taxonomy
//!
//! Events are intentionally *coarse*: one per pipeline stage, not per
//! token or per tool turn. The downstream `aatxe council` is already a
//! 60-minute-per-PR operation; sub-second granularity would drown the
//! TUI and bloat the JSON-Lines log. If finer-grained tracing is ever
//! needed we add it behind a separate `--debug-events` flag.

use crate::types::Severity;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

/// Tagged-union of every event the pipeline emits, in roughly the order
/// they fire. The `kind` discriminator makes the JSON-Lines stream
/// trivially parseable by external readers (`jq -c 'select(.kind ==
/// "proposer_done") | .findings_count'`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CouncilEvent {
    /// Fired once at pipeline entry, before any LLM call.
    Start {
        repo: String,
        pr: u64,
        model: String,
        files_total: u32,
        files_reviewed: u32,
        n_chunks: u32,
    },
    /// Fired before each proposer's LLM call.
    ProposerStart { persona: String, chunk_idx: u32 },
    /// Fired after each proposer's LLM call, success or fail.
    ProposerDone {
        persona: String,
        chunk_idx: u32,
        findings_count: u32,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completion_tokens: Option<u32>,
    },
    /// Fired after the deterministic synthesiser merges proposer
    /// findings — useful for telling "the proposers were noisy but
    /// dedup compressed it" from "dedup was a no-op".
    SynthesizeDone { n_raw: u32, n_deduped: u32 },
    /// Fired before the judge's LLM call. Skipped when there are zero
    /// candidates (the judge call short-circuits in that case).
    JudgeStart { n_candidates: u32 },
    /// Fired after the judge's LLM call, success or fail.
    JudgeDone {
        n_keep: u32,
        n_downgrade: u32,
        n_drop: u32,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Fired once per shippable finding, after the judge stage and the
    /// confidence-floor filter. Surfaces the same fields the rendered
    /// markdown shows so a TUI can present them without re-running the
    /// report renderer.
    FindingEmitted {
        index: u32,
        file: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        severity: Severity,
        category: String,
        title: String,
        confidence: f64,
    },
    /// Fired once at pipeline exit.
    Done {
        total_duration_ms: u64,
        shippable_count: u32,
        critical_count: u32,
        total_prompt_tokens: u32,
        total_completion_tokens: u32,
    },
}

/// Sink consuming a stream of [`CouncilEvent`]s. Must be `Send + Sync`
/// because proposers fire events from `std::thread::scope`-spawned
/// threads. Must be `Debug` because [`crate::pipeline::CouncilOptions`]
/// derives `Debug` and the sink is a field on it.
///
/// Implementations should be cheap: the pipeline emits ~3 events per
/// proposer per chunk, plus a fixed handful of stage-level events. The
/// sink runs *synchronously* on the emitter's thread; a slow sink
/// stalls the pipeline.
pub trait EventSink: Debug + Send + Sync {
    fn emit(&self, event: &CouncilEvent);
}

/// Discards every event. The default for [`CouncilOptions`], so the
/// pipeline's observable behaviour is byte-identical to the
/// pre-streaming era unless the caller opts in.
#[derive(Debug, Clone, Default)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: &CouncilEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory sink used by the pipeline tests. Captures every event
    /// in firing order so a test can assert the full sequence.
    #[derive(Debug, Default)]
    pub struct VecSink {
        pub events: Mutex<Vec<CouncilEvent>>,
    }

    impl EventSink for VecSink {
        fn emit(&self, event: &CouncilEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn null_sink_is_a_no_op() {
        let s = NullSink;
        s.emit(&CouncilEvent::Start {
            repo: "x/y".into(),
            pr: 1,
            model: "stub".into(),
            files_total: 0,
            files_reviewed: 0,
            n_chunks: 0,
        });
    }

    #[test]
    fn events_round_trip_through_serde() {
        let ev = CouncilEvent::ProposerDone {
            persona: "correctness".into(),
            chunk_idx: 0,
            findings_count: 1,
            duration_ms: 42,
            error: None,
            prompt_tokens: Some(123),
            completion_tokens: Some(45),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"kind\":\"proposer_done\""));
        let back: CouncilEvent = serde_json::from_str(&json).unwrap();
        match back {
            CouncilEvent::ProposerDone {
                persona,
                findings_count,
                ..
            } => {
                assert_eq!(persona, "correctness");
                assert_eq!(findings_count, 1);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn finding_emitted_serializes_severity_lowercase() {
        let ev = CouncilEvent::FindingEmitted {
            index: 0,
            file: "src/x.rs".into(),
            line: Some(12),
            severity: Severity::Critical,
            category: "security".into(),
            title: "leaks token".into(),
            confidence: 0.91,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"severity\":\"critical\""),
            "severity must serialise to its lowercase tag: {json}"
        );
    }

    #[test]
    fn vec_sink_captures_in_order_across_threads() {
        let sink = std::sync::Arc::new(VecSink::default());
        std::thread::scope(|s| {
            for i in 0..3 {
                let sink = sink.clone();
                s.spawn(move || {
                    sink.emit(&CouncilEvent::ProposerStart {
                        persona: format!("p{i}"),
                        chunk_idx: i,
                    });
                });
            }
        });
        let captured = sink.events.lock().unwrap();
        assert_eq!(captured.len(), 3, "all three events captured");
    }
}
