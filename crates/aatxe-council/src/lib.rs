//! # aatxe-council
//!
//! Pure orchestration logic for the aatxe agent council — a single-layer
//! mixture-of-agents pull-request reviewer with a dedicated judge.
//!
//! ## Design (research-grounded)
//!
//! Four specialist proposer agents review a PR diff *in parallel*, each with
//! a different persona prompt (correctness, security, performance,
//! maintainability). Their findings are merged by a deterministic synthesizer
//! (dedup + severity normalize + rank), then a *separate* judge agent — the
//! "self-review" stage — confidence-scores every surviving finding and drops
//! the low-confidence ones. The shape is canonical MoA (proposers → judge),
//! not debate: the literature (Du et al. 2023, follow-ups 2025) is mixed on
//! whether multi-round debate beats single-pass proposer→judge for code
//! tasks, and debate triples cost. A dedicated judge — distinct from the
//! proposers — avoids the self-preference bias documented in Zheng et al.
//! 2023 ("Judging LLM-as-a-Judge").
//!
//! ## Crate boundaries
//!
//! This crate is **pure**: no IO, no globals, no HTTP, no spawn. Every side
//! effect (calling the LLM, fetching the PR diff, posting the comment) is
//! injected through traits — [`llm::LlmClient`] for the model, the diff is
//! passed in as a string. The CLI binary at `crates/aatxe` wires this up to
//! Kimi over `ureq` and to GitHub over the same `ureq` client aatxe already
//! uses for sticky comments.
//!
//! ## Pipeline shape
//!
//! ```text
//!   PR diff
//!     │
//!     ▼ diff::parse + diff::filter_ignored
//!     │   (drop lockfiles / generated / vendored before any LLM sees them)
//!     ▼ prompt::build per persona × diff
//!     ▼ pipeline::run_proposers (parallel, via the LlmClient)
//!     ▼ synth::merge (dedup → severity normalize → rank)
//!     ▼ pipeline::run_judge (drops low-confidence; the "self-review")
//!     ▼ report::render_markdown (sticky-marker body)
//!     ▼ posted to GH PR by the CLI
//! ```

pub mod diff;
pub mod llm;
pub mod parse;
pub mod persona;
pub mod pipeline;
pub mod prompt;
pub mod report;
pub mod synth;
pub mod types;

pub use diff::{
    attach_file_contexts, chunk_for_review_with_related, filter_ignored, parse_unified_diff,
    ChunkPolicy, DiffChunk, ParsedFile, RelatedFile,
};
pub use llm::{ChatMessage, ChatRequest, ChatResponse, LlmClient, LlmError, Role};
pub use parse::parse_findings_json;
pub use persona::{judge_system_prompt, persona_system_prompt, Persona};
pub use pipeline::{run_council, run_council_with_files, CouncilOptions};
pub use report::{render_markdown, STICKY_MARKER};
pub use synth::{dedup_and_rank, SynthOptions};
pub use types::{
    AgentReview, CouncilReport, Finding, FindingCategory, JudgeVerdict, JudgedFinding, Severity,
};
