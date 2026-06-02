//! # aatxe-core
//!
//! The brain of aatxe: pure, IO-free logic for statistical microbenchmark
//! comparison. Designed so the [`crate::compare`], [`crate::stats`], and
//! [`crate::report`] modules can each be reasoned about — and tested —
//! independently of the CLI, the network, or the filesystem.
//!
//! Aatxe is the regression-detector. The bench *runners* (TS / Go / Rust SDKs)
//! produce JSON reports conforming to [`types::RunReport`]; aatxe-core does the
//! statistics and the verdict.

pub mod affected;
pub mod compare;
pub mod github;
pub mod report;
pub mod secret;
pub mod stats;
pub mod types;

pub use compare::{compare_reports, has_regressions, CompareOptions};
pub use report::{render_markdown, STICKY_MARKER};
pub use types::{
    AffectedScope, BenchDiff, BenchRun, CompareReport, NeutralReason, RunReport, Verdict,
    SCHEMA_VERSION,
};
