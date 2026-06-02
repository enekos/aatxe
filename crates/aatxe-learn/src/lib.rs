//! # aatxe-learn
//!
//! Self-healing learning corpus for the aatxe agent council. The council
//! posts a sticky markdown comment on every PR; reviewers signal back with
//! reactions, replies, and explicit `aatxe:` directives. This crate distils
//! that signal into a tiny, bounded JSON corpus that:
//!
//! 1. **Persists only the *best* entries** (top-K by score; refuted entries
//!    are evicted; recency-decayed; explicit user directives outweigh
//!    inferred signals).
//! 2. **Self-heals on load** — unknown schema versions are upgraded if
//!    possible; unparseable entries are dropped with a surfaced count;
//!    a missing or empty file produces an empty corpus instead of an error.
//! 3. **Lives as a GitHub Actions artifact** — `aatxe-learning-corpus.json`
//!    is downloaded by the CLI/workflow before the council runs, the
//!    council's prompts are augmented with the most-relevant N entries,
//!    new signals are harvested from the PR's comments + reactions, and
//!    the updated corpus is re-uploaded as the new artifact.
//!
//! ## Design constraints
//!
//! * **Pure.** No IO, no HTTP, no globals. The CLI binary wires the GitHub
//!   API + filesystem in around this crate.
//! * **Bounded.** Hard cap on entry count (default 100). The corpus *cannot*
//!   grow unbounded — if the cap is hit, the lowest-scoring entries are
//!   evicted.
//! * **Forgiving.** Every parse path is lossy: malformed entries are
//!   dropped rather than failing the whole load. The summary surfaces how
//!   many entries were dropped so a reviewer can spot decay.
//! * **Decoupled from the council prompts.** Guidance is rendered as plain
//!   text in [`inject`] — the council crate doesn't depend on this one;
//!   the CLI joins them with a string concat.
//!
//! ## Pipeline shape
//!
//! ```text
//!   previous corpus.json (GH artifact)
//!     │
//!     ▼ load::load_self_healing  (drops malformed entries, surfaces counts)
//!     ▼ inject::build_guidance   (top-K most-relevant → prompt prefix)
//!     │
//!     ├──────── council runs with guidance prefix ────────┐
//!     │                                                    │
//!     ▼ harvest::harvest_pr_feedback (reactions + replies) │
//!     ▼ corpus::merge                                      │
//!     ▼ score::recompute                                   │
//!     ▼ compact::compact            (keep best N)          │
//!     │                                                    │
//!     ▼ new corpus.json (uploaded as the new GH artifact) ◄┘
//! ```

pub mod compact;
pub mod corpus;
pub mod harvest;
pub mod inject;
pub mod load;
pub mod score;
pub mod types;

pub use compact::{compact, CompactOptions};
pub use corpus::{merge_entries, MergeStats};
pub use harvest::{harvest_pr_feedback, HarvestInput, PrComment, Reactions};
pub use inject::{build_guidance, InjectionContext};
pub use load::{load_self_healing, LoadSummary};
pub use score::{score_entry, ScoringOptions};
pub use types::{LearnedEntry, LearningCorpus, SignalKind, CORPUS_SCHEMA_VERSION};
