//! # aatxe-evals
//!
//! Eval harness for aatxe's regression gate. Pure: no IO, no globals.
//!
//! [`stats`] synthesises A/B benchmark-sample pairs with known ground truth
//! (null distribution, known regression, known improvement, controlled noise,
//! between-run drift), runs them through the deterministic
//! [`aatxe_core::compare`] engine, and scores the gate's false-positive rate,
//! true-positive rate, and p-value calibration.
//!
//! Results land in a JSON-serialisable [`report::EvalReport`] that the
//! `aatxe evals` CLI subcommand writes to disk. A baseline JSON can be
//! diffed against the current run with [`report::regressions_against_baseline`]
//! to gate CI on quality regressions exactly the way `aatxe compare`
//! gates on perf regressions.
//!
//! ## Why this exists
//!
//! The unit-test suite proves individual functions behave correctly on hand-
//! picked inputs. Evals prove the *whole comparator* behaves correctly on
//! representative inputs: how often it fires on identical code, and how
//! often it catches a real regression of a given size.

pub mod report;
pub mod stats;

pub use report::{regressions_against_baseline, EvalRegression, EvalReport, EvalTolerances};
pub use stats::{run_stats_evals, StatsEvalSummary, StatsScenario, StatsScenarioResult};
