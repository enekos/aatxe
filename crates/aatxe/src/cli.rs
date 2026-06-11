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
    /// Run the Kimi-backed agent council against a PR and emit a sticky
    /// markdown review. See [`CouncilArgs`] for the full surface.
    Council(CouncilArgs),
    /// Run the eval harness — stats engine accuracy on synthetic A/B
    /// pairs and council quality on labeled diff fixtures. Optionally
    /// gates CI by diffing against a baseline.
    Evals(EvalsArgs),
    /// Manage the self-healing learning corpus — harvest fresh feedback
    /// from a PR, compact the corpus to its bounded best-N, or show its
    /// current state. Persisted as a GitHub Actions artifact between
    /// council runs.
    Learn(LearnArgs),
    /// Local perf-vs workflow: materialize a sibling worktree at the given
    /// ref, run aatxe's own benches (council / big-diff) on both sides,
    /// and compare via `aatxe-core::compare_reports`. Replaces the
    /// commit → push → wait-on-GH-Actions loop with a ~30 s local one.
    #[command(name = "perf-vs")]
    PerfVs(PerfVsArgs),
    /// Manage locally-saved baseline RunReports. `aatxe baseline save`
    /// snapshots a report under `.aatxe/baselines/`; `aatxe compare
    /// --against-local` then uses it as the base side — a trial-locally
    /// loop that needs no CI artifacts.
    Baseline(BaselineArgs),
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum PerfBenchArg {
    /// `examples/council-bench` — 9 micros covering the council's pure
    /// pipeline (diff parse, filter, chunk, prompt build, JSON parse,
    /// synth, end-to-end with stub). ~5 s.
    Council,
    /// `examples/big-diff-bench` — large-diff parse cost. ~30 s.
    BigDiff,
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
    /// Which bench(es) to run. Default `council` (fast).
    #[arg(long, value_enum, default_value_t = PerfBenchArg::Council)]
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
    /// p-value cutoff for the Mann–Whitney U test (default 0.05).
    #[arg(long, default_value_t = 0.05)]
    pub alpha: f64,
    /// CV cutoff above which the noise gate engages (default 0.25 = 25%).
    #[arg(long, default_value_t = 0.25)]
    pub noisy_cv: f64,
    /// Stream bench stdout to the caller as it runs (default off — only
    /// the final summary prints).
    #[arg(long)]
    pub verbose: bool,
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
    /// Bench discovery globs (passed to the adapter). Defaults are language-specific.
    pub patterns: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct CompareArgs {
    /// Base-side RunReport JSON. Mutually exclusive with `--against-local`.
    #[arg(
        long,
        required_unless_present = "against_local",
        conflicts_with = "against_local"
    )]
    pub base: Option<PathBuf>,
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
    #[arg(long)]
    pub head: PathBuf,
    /// JSON output path for the CompareReport. Default `./aatxe-report.json`.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Markdown render path. If set, render the sticky body to this file.
    #[arg(long)]
    pub markdown: Option<PathBuf>,
    /// Median-delta threshold for "meaningful" change (default 0.05 = 5%).
    #[arg(long, default_value_t = 0.05)]
    pub threshold: f64,
    /// p-value cutoff for the Mann–Whitney U test (default 0.05).
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

/// `aatxe council review` — fetch a PR diff, run the four-persona Kimi
/// council with a dedicated judge, render a sticky markdown comment, and
/// optionally post it.
///
/// The default flow is one-shot: fetch → review → render → (optionally)
/// post. If you want to inspect or hand-edit the review before posting,
/// pass `--markdown <file>` *without* `--post` and run `aatxe comment`
/// separately, or rerun the council with `--post` once you're happy with
/// the body.
#[derive(clap::Args, Debug)]
pub struct CouncilArgs {
    /// Path to a unified-diff file. Mutually exclusive with `--pr`; when
    /// both are absent the CLI tries to read the diff from stdin.
    #[arg(long)]
    pub diff_file: Option<PathBuf>,
    /// PR number to fetch from GitHub. Falls back to detection from
    /// `GITHUB_REF` / `AATXE_PR` env vars (same logic as `aatxe comment`).
    #[arg(long)]
    pub pr: Option<u64>,
    /// Repo slug `owner/name`. Defaults to `GITHUB_REPOSITORY`.
    #[arg(long, env = "GITHUB_REPOSITORY")]
    pub repo: Option<String>,
    /// GitHub token. Defaults to `GITHUB_TOKEN` / `GH_TOKEN`.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub token: Option<String>,
    /// Override the GH API base.
    #[arg(long, env = "GITHUB_API_URL")]
    pub api_base: Option<String>,
    /// Override the council model (default `kimi-k2.6`, also reads
    /// `KIMI_MODEL`).
    #[arg(long, env = "KIMI_MODEL")]
    pub model: Option<String>,
    /// Confidence floor for the judge: findings below this are hidden
    /// from the rendered comment. Default 0.55.
    #[arg(long, default_value_t = 0.55)]
    pub confidence_floor: f64,
    /// Extra path patterns to ignore in addition to the built-in lockfile
    /// / generated-code list. Repeatable.
    #[arg(long = "ignore")]
    pub extra_ignored: Vec<String>,
    /// Persist the full [`aatxe_council::CouncilReport`] (raw + judged
    /// findings + telemetry) as JSON. Useful for downstream tooling.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Persist the rendered sticky markdown body to this file. Defaults
    /// to not writing markdown to disk when `--post` is set.
    #[arg(long)]
    pub markdown: Option<PathBuf>,
    /// Post (or update) the sticky council comment on the PR.
    #[arg(long)]
    pub post: bool,
    /// Exit code 2 if any shippable finding has severity `critical`.
    /// Mirrors the perf gate's `--fail-on-regression` flag.
    #[arg(long)]
    pub fail_on_critical: bool,
    /// Path to the learning-corpus JSON (produced by `aatxe learn
    /// harvest`). When set, the top-K most-relevant entries are rendered
    /// into a guidance block prepended to every proposer + judge system
    /// prompt. The corpus is loaded with full self-healing semantics —
    /// a missing or malformed file degrades to no guidance, never an
    /// error. Recommended path in CI: `aatxe-learning-corpus.json`.
    #[arg(long)]
    pub learning_corpus: Option<PathBuf>,
    /// Maximum number of corpus entries to include in the guidance
    /// prefix. Default 8.
    #[arg(long, default_value_t = 8)]
    pub learning_max_entries: usize,
    /// Path or executable name of the `pi` binary used to drive the
    /// council. Defaults to `pi` on `$PATH`. The council shells out to
    /// `pi` per LLM call so each proposer can read/grep/find/ls the repo
    /// being reviewed; `KIMI_API_KEY` is forwarded to the subprocess.
    #[arg(long, env = "PI_BIN")]
    pub pi_binary: Option<PathBuf>,
    /// Which LLM backend the council drives. `pi-proxy` (default) shells
    /// out to the locally-installed `pi` agent against the Kimi-coding
    /// endpoint. `claude-code` shells out to the locally-installed
    /// `claude` CLI and uses the engineer's Claude Code
    /// subscription/auth — no separate API key needed.
    #[arg(long, value_enum, default_value_t = BackendArg::PiProxy)]
    pub backend: BackendArg,
    /// Path or executable name of the `claude` binary used when
    /// `--backend=claude-code`. Defaults to `claude` on `$PATH`.
    #[arg(long, env = "CLAUDE_BIN")]
    pub claude_binary: Option<PathBuf>,
    /// Stream pipeline events as JSON Lines to this path while the
    /// council runs. Use `-` for stdout. Each line is a self-contained
    /// `CouncilEvent` JSON object (proposer_start, finding_emitted,
    /// judge_done, etc) — see [`aatxe_council::events`]. Useful for
    /// piping a long-running run into a TUI or for post-hoc debugging.
    #[arg(long)]
    pub json_events: Option<String>,
    /// Pause after the judge stage and interactively curate findings
    /// before rendering / posting. For each shippable finding the user
    /// picks `[k]eep / [d]rop / [s]kip-all`. Defaults to on when stdin
    /// is a TTY *and* `--post` is set; off otherwise (so headless CI
    /// keeps its existing one-shot behaviour). `--interactive=false`
    /// forces it off; `--interactive=true` forces it on regardless of
    /// TTY (useful when stdin is a pipe but you still want the prompt).
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub interactive: Option<bool>,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum BackendArg {
    /// Shell out to the local `pi` agent CLI (Kimi-coding endpoint).
    PiProxy,
    /// Shell out to the local `claude` CLI (Claude Code subscription).
    ClaudeCode,
}

/// `aatxe evals` — run the eval harness.
///
/// Two surfaces, runnable independently or together:
/// * `--stats` — synthetic A/B benchmark pairs with known ground truth.
///   Fully deterministic, no network. Default-on.
/// * `--council` — labeled PR diff fixtures from `evals/council/cases/`.
///   Default-on. Uses the stub LLM unless `--council-real-llm` is passed,
///   which then requires `KIMI_API_KEY` in the environment.
///
/// The harness writes a JSON [`aatxe_evals::EvalReport`] and an optional
/// markdown summary. When `--baseline <path>` is set the report is
/// diffed against the baseline and the gate fires (exit 2) on regression
/// past tolerance — symmetric to `aatxe compare --fail-on-regression`.
#[derive(clap::Args, Debug)]
pub struct EvalsArgs {
    /// Enable the council eval. Defaults to true.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub council: bool,
    /// Enable the stats eval. Defaults to true.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub stats: bool,
    /// Council eval corpus directory. Defaults to `evals/council/cases`.
    #[arg(long)]
    pub corpus: Option<PathBuf>,
    /// Use real Kimi for the council eval instead of the stub. Requires
    /// `KIMI_API_KEY`. Off by default — every CI PR runs the stub path
    /// because real Kimi is non-free and non-deterministic.
    #[arg(long)]
    pub council_real_llm: bool,
    /// Override the confidence floor used while scoring (default 0.55).
    #[arg(long, default_value_t = 0.55)]
    pub confidence_floor: f64,
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
    /// Don't fail on regression; just print the report. Useful when
    /// iterating on the corpus.
    #[arg(long)]
    pub no_fail: bool,
    /// Which LLM backend to use when `--council-real-llm` is set.
    /// Same shape as `aatxe council --backend`. Default `pi-proxy`.
    #[arg(long, value_enum, default_value_t = BackendArg::PiProxy)]
    pub backend: BackendArg,

    /// Offline confidence-floor recalibration: load a prior eval JSON
    /// (which must contain per-finding records) and re-derive the
    /// council metrics at each floor in `--recalibrate-floors`. Skips
    /// the LLM-running path entirely — purely a math sweep over cached
    /// records. Mutually exclusive with `--council-real-llm`.
    ///
    /// Use this to choose a new default floor from a single real-LLM
    /// run instead of re-running the LLM once per candidate floor.
    #[arg(long, conflicts_with = "council_real_llm")]
    pub recalibrate_from: Option<PathBuf>,

    /// Comma-separated list of floors to sweep when
    /// `--recalibrate-from` is set. Default `0.55,0.60,0.65` — the
    /// three points the calibrate-confidence-floor.sh script also
    /// uses. The first entry is treated as the baseline; deltas are
    /// printed relative to it.
    #[arg(long, default_value = "0.55,0.60,0.65", value_delimiter = ',')]
    pub recalibrate_floors: Vec<f64>,
}

/// `aatxe learn` — manage the learning corpus.
///
/// The corpus is a single JSON file persisted between council runs as a
/// GitHub Actions artifact (`aatxe-learning-corpus`). It contains *only*
/// the highest-signal entries: explicit user directives written as
/// `aatxe: remember <…>` comments, plus confirmations/refutations from
/// reactions and `aatxe: good catch / false-positive` directives.
#[derive(clap::Args, Debug)]
pub struct LearnArgs {
    #[command(subcommand)]
    pub command: LearnCommand,
}

#[derive(Subcommand, Debug)]
pub enum LearnCommand {
    /// Pull the PR's comments + reactions, harvest signals, merge into the
    /// corpus, compact, and write the result back. Self-healing: a
    /// missing / malformed corpus on disk becomes a fresh empty corpus
    /// with a surfaced summary, never an error.
    Harvest(LearnHarvestArgs),
    /// Recompute scores, drop below-threshold entries, sort + truncate to
    /// the keep-best-N cap. Useful to run on a schedule or after a corpus
    /// hand-edit.
    Compact(LearnCompactArgs),
    /// Print a human-readable summary of the corpus on disk.
    Show(LearnShowArgs),
}

#[derive(clap::Args, Debug)]
pub struct LearnHarvestArgs {
    /// Path to the corpus JSON on disk. Reads existing contents (or
    /// starts fresh if missing/invalid) and writes the merged + compacted
    /// result back to the same path.
    #[arg(long, default_value = "aatxe-learning-corpus.json")]
    pub corpus: PathBuf,
    /// PR to harvest signals from. Falls back to detection from
    /// `GITHUB_REF` / `AATXE_PR`.
    #[arg(long)]
    pub pr: Option<u64>,
    /// Repo slug `owner/name`. Defaults to `GITHUB_REPOSITORY`.
    #[arg(long, env = "GITHUB_REPOSITORY")]
    pub repo: Option<String>,
    /// GitHub token. Defaults to `GITHUB_TOKEN` / `GH_TOKEN`.
    #[arg(long, env = "GITHUB_TOKEN")]
    pub token: Option<String>,
    /// Override the GH API base.
    #[arg(long, env = "GITHUB_API_URL")]
    pub api_base: Option<String>,
    /// Bot login to ignore when scanning for `aatxe:` directives.
    /// Defaults to `github-actions[bot]` which is what the council posts as.
    #[arg(long, default_value = "github-actions[bot]")]
    pub bot_login: String,
    /// Allowlist of GitHub `author_association` values whose `aatxe:`
    /// directives are honoured. Defaults to OWNER + MEMBER + COLLABORATOR
    /// (i.e. write access) when not provided. Pass to opt CONTRIBUTOR in
    /// for non-org repos where a solo maintainer needs to plant guidance.
    #[arg(long, value_delimiter = ',')]
    pub trusted_associations: Vec<String>,
    /// Path to the latest council report JSON (from `aatxe council --out`).
    /// Used to resolve `aatxe: good catch on N` directives by index.
    /// Optional — without it, only `aatxe: remember <…>` directives and
    /// reaction-based signals against the top shipped finding can be
    /// harvested (the index-based directives are silently skipped).
    #[arg(long)]
    pub council_report: Option<PathBuf>,
    /// Pre-fetched comments JSON. Skips the GH API call entirely — useful
    /// for testing and for offline reproductions. Expected shape is a
    /// JSON array of `PrComment` objects.
    #[arg(long)]
    pub comments_file: Option<PathBuf>,
    /// Cap the corpus to this many entries after compaction. Default 100.
    #[arg(long, default_value_t = 100)]
    pub max_entries: usize,
    /// Drop entries scoring below this threshold during compaction.
    /// Default 0.1.
    #[arg(long, default_value_t = 0.1)]
    pub min_score: f64,
}

#[derive(clap::Args, Debug)]
pub struct LearnCompactArgs {
    #[arg(long, default_value = "aatxe-learning-corpus.json")]
    pub corpus: PathBuf,
    #[arg(long, default_value_t = 100)]
    pub max_entries: usize,
    #[arg(long, default_value_t = 0.1)]
    pub min_score: f64,
}

#[derive(clap::Args, Debug)]
pub struct LearnShowArgs {
    #[arg(long, default_value = "aatxe-learning-corpus.json")]
    pub corpus: PathBuf,
    /// Render as JSON instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
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
