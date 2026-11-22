//! CLI surface, declared with `clap` derive. Keep this file free of
//! business logic — it only describes the shape of the command-line.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "aatxe",
    version,
    about = "Statistical microbenchmark + regression gate for TS, Go, and Rust.",
    long_about = "Aatxe runs your benches, statistically compares head against base, \
                  posts a sticky GitHub PR comment, and gates CI on regressions."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Discover and execute benches, emitting a JSON RunReport.
    Run(RunArgs),
    /// Compare two RunReports and emit a CompareReport.
    Compare(CompareArgs),
    /// Render a CompareReport as a sticky Markdown PR comment body.
    Report(ReportArgs),
    /// Post (or update) a sticky comment with a rendered report body.
    Comment(CommentArgs),
    /// Print the affected set of bench files for a given diff base.
    Affected(AffectedArgs),
    /// Discovery preview: list bench files that would run.
    List(ListArgs),
    /// Run the eval harness — stats engine accuracy on synthetic A/B
    /// pairs with known ground truth. Optionally gates CI by diffing
    /// against a baseline.
    Evals(EvalsArgs),
    /// Local perf-vs workflow: materialize a sibling worktree at the given
    /// ref, run aatxe's own benches (core / ast) on both sides,
    /// and compare via `aatxe-core::compare_reports`. Replaces the
    /// commit → push → wait-on-GH-Actions loop with a ~30 s local one.
    #[command(name = "perf-vs")]
    PerfVs(PerfVsArgs),
    /// Manage locally-saved baseline RunReports. `aatxe baseline save`
    /// snapshots a report under `.aatxe/baselines/`; `aatxe compare
    /// --against-local` then uses it as the base side — a trial-locally
    /// loop that needs no CI artifacts.
    Baseline(BaselineArgs),
    /// Launch the local realtime dashboard: spawn coding agents in
    /// isolated worktrees, re-bench every working-tree change, and watch
    /// head-vs-base trajectories (plus tournament standings) stream live
    /// into the browser.
    #[cfg(feature = "ui")]
    Ui(UiArgs),
}

/// Where benches execute — the backend for `--isolation`.
#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum IsolationArg {
    /// Run on the host, directly. Fast, but subject to host noise
    /// (background load, thermal throttling, other processes).
    Local,
    /// Run inside an ephemeral libkrun microVM via `krunvm` — pinned
    /// vCPUs, fixed RAM, clean rootfs — for reproducible numbers. Needs
    /// `krunvm` installed (`make microvm-setup`); works on macOS and Linux.
    Microvm,
}

/// Shared microVM knobs, flattened into every bench-running subcommand.
/// Only the parameters that meaningfully change results are surfaced;
/// everything else uses libkrun defaults.
#[derive(clap::Args, Debug, Clone)]
pub struct VmOpts {
    /// Where to run the bench. `local` (default) or `microvm`.
    #[arg(long, value_enum, default_value_t = IsolationArg::Local)]
    pub isolation: IsolationArg,
    /// microVM vCPUs (only with `--isolation microvm`).
    #[arg(long, env = "AATXE_VM_CPUS", default_value_t = crate::sandbox::DEFAULT_VM_CPUS)]
    pub vm_cpus: u32,
    /// microVM RAM in MiB (only with `--isolation microvm`).
    #[arg(long, env = "AATXE_VM_MEM", default_value_t = crate::sandbox::DEFAULT_VM_MEM_MIB)]
    pub vm_mem: u32,
    /// OCI image for the guest (must ship the language toolchain). Defaults
    /// per language: `rust:1` / `golang:1` / `node:22`.
    #[arg(long, env = "AATXE_VM_IMAGE")]
    pub vm_image: Option<String>,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum PerfBenchArg {
    /// `examples/core-bench` — the `aatxe-core` statistical brain:
    /// summarize_samples, Mann-Whitney U, MAD, Welch-t, compare_reports,
    /// and the affected-set import extractor. ~5 s.
    Core,
    /// `examples/ast-bench` — `aatxe-ast` tree-sitter parse cost across
    /// Rust/TS/Go fixtures. ~5 s.
    Ast,
    /// Run every supported bench and concatenate the results into one
    /// `RunReport` per side before comparing.
    All,
}

/// `aatxe perf-vs` — local A/B perf comparison across two worktrees.
///
/// Materializes a sibling worktree at `<worktree-dir>/<ref-slug>`,
/// builds + runs the chosen bench(es) in both HEAD and that worktree,
/// and diffs the resulting `RunReport`s with the same comparator the
/// CI gate uses. The worktree is reused between runs (cheap rebuild)
/// unless `--rm-worktree` is set.
#[derive(clap::Args, Debug)]
pub struct PerfVsArgs {
    /// Git ref to compare HEAD against. Anything `git rev-parse` accepts:
    /// branch, tag, sha, `HEAD~3`, `origin/master`.
    #[arg(long)]
    pub against: String,
    /// Which bench(es) to run. Default `core`.
    #[arg(long, value_enum, default_value_t = PerfBenchArg::Core)]
    pub bench: PerfBenchArg,
    /// Parent directory for sibling worktrees. Default
    /// `<repo>/../aatxe-worktrees`. Each `--against` ref gets its own
    /// subdirectory named after the resolved short SHA.
    #[arg(long)]
    pub worktree_dir: Option<PathBuf>,
    /// Remove the worktree after the run. Off by default so subsequent
    /// runs against the same ref skip the `git worktree add` + initial
    /// build cost.
    #[arg(long)]
    pub rm_worktree: bool,
    /// Directory for intermediate JSON + markdown output. Default
    /// `<repo>/tmp/perf-vs/<ref-slug>-<bench>`.
    #[arg(long)]
    pub out_dir: Option<PathBuf>,
    /// Exit non-zero (code 2) when any bench regresses past
    /// `--threshold`. Mirrors `aatxe compare --fail-on-regression`.
    #[arg(long)]
    pub fail_on_regression: bool,
    /// Median-delta threshold for "meaningful" change (default 0.05 = 5%).
    #[arg(long, default_value_t = 0.05)]
    pub threshold: f64,
    /// p-value cutoff for the significance test (default 0.05).
    #[arg(long, default_value_t = 0.05)]
    pub alpha: f64,
    /// CV cutoff above which the noise gate engages (default 0.25 = 25%).
    #[arg(long, default_value_t = 0.25)]
    pub noisy_cv: f64,
    /// Independent process runs per side, interleaved base/head (ABBA order)
    /// so machine drift hits both sides equally. With ≥ 4 the verdict is
    /// computed across rounds; 1 restores the old single-run behaviour.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=50))]
    pub rounds: u32,
    /// Stream bench stdout to the caller as it runs (default off — only
    /// the final summary prints).
    #[arg(long)]
    pub verbose: bool,
    /// microVM isolation knobs (`--isolation microvm` for predictable
    /// numbers). Both sides of the A/B run in matching guests.
    #[command(flatten)]
    pub vm: VmOpts,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum LangArg {
    Ts,
    Go,
    Rust,
}

impl LangArg {
    pub fn to_core(self) -> aatxe_core::types::Language {
        match self {
            LangArg::Ts => aatxe_core::types::Language::Ts,
            LangArg::Go => aatxe_core::types::Language::Go,
            LangArg::Rust => aatxe_core::types::Language::Rust,
        }
    }
}

#[derive(clap::Args, Debug)]
pub struct RunArgs {
    /// Language adapter to drive.
    #[arg(long, value_enum)]
    pub lang: LangArg,
    /// Working directory of the service. Defaults to the current directory.
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Output path for the resulting RunReport JSON. Default `./aatxe.json`.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Service name to embed in the report. Defaults to the cwd's basename.
    #[arg(long)]
    pub service: Option<String>,
    /// Git ref the run is benching. Defaults to `HEAD`'s short SHA.
    #[arg(long)]
    pub r#ref: Option<String>,
    /// Filter bench names by regex (applied client-side by the adapter).
    #[arg(long)]
    pub filter: Option<String>,
    /// Restrict to benches affected by `--base`; emit `affectedScope` metadata.
    #[arg(long)]
    pub affected: bool,
    /// Base ref for `--affected`. Required if `--affected`.
    #[arg(long)]
    pub base: Option<String>,
    /// Print runner stdout to the caller as it produces it.
    #[arg(long)]
    pub verbose: bool,
    /// microVM isolation knobs. `--isolation microvm` runs the language
    /// runner inside a libkrun guest for predictable numbers.
    #[command(flatten)]
    pub vm: VmOpts,
    /// Bench discovery globs (passed to the adapter). Defaults are language-specific.
    pub patterns: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct CompareArgs {
    /// Base-side RunReport JSON. Repeat it (or pass several paths) to pool
    /// independent runs ("rounds"); with ≥ 4 rounds on both sides the verdict
    /// is computed across rounds instead of samples. Mutually exclusive with
    /// `--against-local`.
    #[arg(
        long,
        num_args = 1..,
        required_unless_present = "against_local",
        conflicts_with = "against_local"
    )]
    pub base: Vec<PathBuf>,
    /// Use the locally-saved baseline (see `aatxe baseline save`) as the
    /// base side instead of `--base`.
    #[arg(long)]
    pub against_local: bool,
    /// Which named local baseline to compare against. Only meaningful with
    /// `--against-local`.
    #[arg(long, default_value = "default")]
    pub baseline_name: String,
    /// Override the baseline directory. Default `<repo-root>/.aatxe/baselines`
    /// (falls back to the current directory outside a git repo).
    #[arg(long)]
    pub baseline_dir: Option<PathBuf>,
    /// Head-side RunReport JSON. Repeatable like `--base`.
    #[arg(long, num_args = 1.., required = true)]
    pub head: Vec<PathBuf>,
    /// JSON output path for the CompareReport. Default `./aatxe-report.json`.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Markdown render path. If set, render the sticky body to this file.
    #[arg(long)]
    pub markdown: Option<PathBuf>,
    /// Median-delta threshold for "meaningful" change (default 0.05 = 5%).
    #[arg(long, default_value_t = 0.05)]
    pub threshold: f64,
    /// p-value cutoff for the significance test (default 0.05).
    #[arg(long, default_value_t = 0.05)]
    pub alpha: f64,
    /// CV cutoff above which the noise gate engages (default 0.25 = 25%).
    #[arg(long, default_value_t = 0.25)]
    pub noisy_cv: f64,
    /// Exit non-zero (code 2) when any bench regresses.
    #[arg(long)]
    pub fail_on_regression: bool,
}

#[derive(clap::Args, Debug)]
pub struct ReportArgs {
    /// Path to a CompareReport JSON (as produced by `aatxe compare`).
    #[arg(long)]
    pub diff: PathBuf,
    /// Output path for the markdown body. Default `./aatxe-report.md`.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct CommentArgs {
    /// Path to a rendered markdown body (must contain the sticky marker).
    #[arg(long)]
    pub report: PathBuf,
    /// Repo slug `owner/name`. Defaults to `GITHUB_REPOSITORY`.
    #[arg(long, env = "GITHUB_REPOSITORY")]
    pub repo: Option<String>,
    /// Pull request number. Defaults to detection from CI env.
    #[arg(long)]
    pub pr: Option<u64>,
    /// GitHub token. Defaults to `GITHUB_TOKEN` or `GH_TOKEN`.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub token: Option<String>,
    /// Override the GH API base (for self-hosted enterprise).
    #[arg(long, env = "GITHUB_API_URL")]
    pub api_base: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct AffectedArgs {
    #[arg(long, value_enum)]
    pub lang: LangArg,
    #[arg(long)]
    pub base: String,
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Print *all* discovered bench files, marking affected ones with `*`.
    #[arg(long)]
    pub show_all: bool,
    pub patterns: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct ListArgs {
    #[arg(long, value_enum)]
    pub lang: LangArg,
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    pub patterns: Vec<String>,
}

/// `aatxe evals` — run the stats eval harness: synthetic A/B benchmark
/// pairs with known ground truth. Fully deterministic, no network.
///
/// The harness writes a JSON [`aatxe_evals::EvalReport`] and an optional
/// markdown summary. When `--baseline <path>` is set the report is
/// diffed against the baseline and the gate fires (exit 2) on regression
/// past tolerance — symmetric to `aatxe compare --fail-on-regression`.
#[derive(clap::Args, Debug)]
pub struct EvalsArgs {
    /// Write the JSON [`aatxe_evals::EvalReport`] here. Default
    /// `./aatxe-evals.json`.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Optional markdown summary path. Useful for sticky comments.
    #[arg(long)]
    pub markdown: Option<PathBuf>,
    /// Compare the result against this JSON baseline. If any metric
    /// regresses past tolerance the command exits 2 (matches the perf
    /// gate's contract).
    #[arg(long)]
    pub baseline: Option<PathBuf>,
    /// Don't fail on regression; just print the report.
    #[arg(long)]
    pub no_fail: bool,
}

/// `aatxe baseline` — snapshot RunReports locally so `aatxe compare
/// --against-local` works without CI artifacts.
///
/// The intended loop on a consumer repo:
///
/// ```text
/// aatxe run --lang ts                 # bench current code → ./aatxe.json
/// aatxe baseline save                 # snapshot it as the local baseline
/// <edit code>
/// aatxe run --lang ts
/// aatxe compare --against-local --head aatxe.json
/// ```
///
/// Baselines live under `<repo-root>/.aatxe/baselines/<name>.json`. The
/// `.aatxe/` directory is self-gitignoring (a `.gitignore` containing `*`
/// is written on first save) — local baselines are per-machine state and
/// never belong in the repo.
#[derive(clap::Args, Debug)]
pub struct BaselineArgs {
    #[command(subcommand)]
    pub command: BaselineCommand,
}

#[derive(Subcommand, Debug)]
pub enum BaselineCommand {
    /// Validate a RunReport JSON and snapshot it as a named local baseline.
    Save(BaselineSaveArgs),
    /// Print a summary of a saved baseline (service, ref, per-bench medians).
    Show(BaselineShowArgs),
    /// List every saved baseline with its ref and bench count.
    List(BaselineListArgs),
    /// Delete a saved baseline.
    Rm(BaselineRmArgs),
}

#[derive(clap::Args, Debug)]
pub struct BaselineSaveArgs {
    /// RunReport JSON to snapshot (as produced by `aatxe run`).
    #[arg(long, default_value = "./aatxe.json")]
    pub report: PathBuf,
    /// Baseline name. Use distinct names to keep several baselines around
    /// (e.g. one per branch or per experiment).
    #[arg(long, default_value = "default")]
    pub name: String,
    /// Override the baseline directory. Default `<repo-root>/.aatxe/baselines`
    /// (falls back to the current directory outside a git repo).
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct BaselineShowArgs {
    #[arg(long, default_value = "default")]
    pub name: String,
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Print the raw RunReport JSON instead of the summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, Debug)]
pub struct BaselineListArgs {
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct BaselineRmArgs {
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

/// `aatxe ui` — local realtime dashboard.
///
/// Serves an embedded single-page frontend on localhost. Three layers,
/// each usable without the next: (1) a live `RunReport`/`CompareReport`
/// sink (`POST /api/runs`, plus a watcher on `tmp/perf-vs/` and
/// `.aatxe/baselines/`); (2) coding agents spawned into isolated
/// worktrees whose benches re-run on every working-tree change; (3)
/// tournaments ranking K agents on the same task.
#[cfg(feature = "ui")]
#[derive(clap::Args, Debug)]
pub struct UiArgs {
    /// Port to bind on 127.0.0.1.
    #[arg(long, default_value_t = 4866)]
    pub port: u16,
    /// Repository the dashboard operates on. Defaults to the enclosing
    /// repo of the current directory.
    #[arg(long)]
    pub repo: Option<PathBuf>,
    /// Ref agents branch from and bench-compare against.
    #[arg(long, default_value = "HEAD")]
    pub base: String,
    /// Which of aatxe's own benches to run per iteration. Ignored when
    /// `--bench-cmd` is set.
    #[arg(long, value_enum, default_value_t = PerfBenchArg::Core)]
    pub bench: PerfBenchArg,
    /// Arbitrary shell command whose stdout ends with a RunReport JSON.
    /// This is what makes the dashboard work in any repo with an aatxe
    /// SDK: e.g. `--bench-cmd "npx aatxe-ts-runner …"`.
    #[arg(long)]
    pub bench_cmd: Option<String>,
    /// Parent directory for agent + base worktrees. Default
    /// `<repo>/../aatxe-worktrees` (same convention as `perf-vs`).
    #[arg(long)]
    pub worktree_dir: Option<PathBuf>,
    /// Seconds between dirty-checks of each agent worktree.
    #[arg(long, default_value_t = 15)]
    pub poll_secs: u64,
    /// Which backend drives spawned coding agents.
    #[arg(long, value_enum, default_value_t = UiAgentArg::Claude)]
    pub agent_backend: UiAgentArg,
    /// Model for the gemini backend (default `gemini-2.5-flash`).
    #[arg(long, env = "GEMINI_MODEL")]
    pub gemini_model: Option<String>,
    /// Path or executable name of the `claude` binary for agent runs.
    #[arg(long, env = "CLAUDE_BIN")]
    pub claude_binary: Option<PathBuf>,
    /// Model alias passed to the agent (`--model sonnet`, `opus`, …).
    #[arg(long, env = "CLAUDE_MODEL")]
    pub model: Option<String>,
    /// Don't auto-open the browser.
    #[arg(long)]
    pub no_open: bool,
    /// Maximum concurrently-running agents.
    #[arg(long, default_value_t = 6)]
    pub max_agents: usize,
    /// Median-delta threshold for "meaningful" change (default 0.05 = 5%).
    #[arg(long, default_value_t = 0.05)]
    pub threshold: f64,
    /// p-value cutoff for the significance test (default 0.05).
    #[arg(long, default_value_t = 0.05)]
    pub alpha: f64,
    /// CV cutoff above which the noise gate engages (default 0.25 = 25%).
    #[arg(long, default_value_t = 0.25)]
    pub noisy_cv: f64,
}

#[cfg(feature = "ui")]
#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum UiAgentArg {
    /// Shell out to the local `claude` CLI in print mode (subscription auth).
    Claude,
    /// Native tool-use loop over the Gemini API. Requires `GEMINI_API_KEY`.
    Gemini,
    /// Deterministic offline scripted runner — demos and harness checks.
    Stub,
}
