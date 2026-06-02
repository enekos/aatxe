//! # aatxe-evals
//!
//! Eval harness for aatxe. Two surfaces, both pure (no IO, no globals):
//!
//! 1. **Council evals** ([`council`]) — load a labeled corpus of unified-diff
//!    fixtures, each annotated with the findings the council *must* surface
//!    and the file paths it *must not* flag. Run the council pipeline (real
//!    or stubbed) against every case and score precision / recall / F1 per
//!    severity, plus calibration of the judge's confidence scores.
//!
//! 2. **Stats evals** ([`stats`]) — synthesise A/B benchmark-sample pairs with
//!    known ground truth (null distribution, known regression, known
//!    improvement, controlled noise), run them through the deterministic
//!    [`aatxe_core::compare`] engine, and score the regression gate's
//!    false-positive rate, true-positive rate, and p-value calibration.
//!
//! Both surfaces share a JSON-serialisable [`report::EvalReport`] that the
//! `aatxe evals` CLI subcommand writes to disk. A baseline JSON can be
//! diffed against the current run with [`report::regressions_against_baseline`]
//! to gate CI on quality regressions exactly the way `aatxe compare`
//! gates on perf regressions.
//!
//! ## Why this exists
//!
//! The unit-test suite proves individual functions behave correctly on hand-
//! picked inputs. Evals prove the *whole pipeline* behaves correctly on
//! representative inputs — the swe-bench / HumanEval shape adapted to a
//! statistical-comparator + LLM-PR-reviewer hybrid. They are the thing that
//! tells us "the council catches password-leak PRs" with a number, not a
//! vibe.

pub mod council;
pub mod report;
pub mod stats;

pub use council::{
    score_council, CouncilCase, CouncilEvalOptions, CouncilEvalSummary, ExpectedFinding,
    ForbiddenPath,
};
pub use report::{regressions_against_baseline, EvalRegression, EvalReport, EvalTolerances};
pub use stats::{run_stats_evals, StatsEvalSummary, StatsScenario, StatsScenarioResult};
