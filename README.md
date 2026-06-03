# aatxe

> Statistical microbenchmark + regression-gate harness for TypeScript, Go, and Rust.

**Aatxe** [/ˈaːtʃe/] is the red-bull spirit of Basque mythology that emerges
from caves at night to identify and punish wrongdoers. The name is fitting:
this tool benches your code on every PR, statistically compares head against
base, posts a sticky comment, and gates CI when something regressed.

```
                ┌──────────────────────┐
PR pushed  ─┬──▶│  GH Actions: HEAD     │ ─┐
            │   └──────────────────────┘  │       ┌────────────┐       ┌──────────────┐
            │   ┌──────────────────────┐  ├─▶ Compare ─▶ Sticky │ ─▶ │  GH PR comment │
            └──▶│  GH Actions: base     │ ─┘  (median Δ +    PR comment │  (in-place)   │
                └──────────────────────┘     MW-U + noise gate)        └──────────────┘
```

No LLM. No magic instrumentation. Statistics, not opinions. Verdicts come
from numbers, not text generation.

A separate, **opt-in** subsystem — the [agent council](#agent-council) —
layers semantic PR review on top, with its own sticky marker and its own
exit-code gate. The perf gate above is untouched by it.

## Why this rebuild

Aatxe is a clean-slate Rust rebuild of an older Node-only perf-diff tool,
with three sharper goals:

* **Polyglot at the boundary.** Aatxe defines a single JSON `RunReport`
  schema; per-language SDKs (`@aatxe/bench` for TS, `aatxe-bench` for Rust,
  the `aatxe` Go module) produce it. The Rust CLI handles comparison,
  rendering, the sticky comment, and the affected-set resolver — the
  *hard* parts only need to exist once.
* **GitHub Actions first-class.** Workflows ship in `.github/workflows/`,
  including a reusable `aatxe.yml` that downstream services can call.
* **Testable end-to-end.** The core (`aatxe-core`) is pure — no IO, no
  globals — and ships 52 unit + integration tests covering stats, the
  three-signal verdict, markdown rendering, the affected-set graph across
  all three languages, and the GitHub URL/header protocol.

## Hacking on aatxe

Everything routes through `make`. Run `make help` for the full list; the
load-bearing ones:

```
make check        # cargo fmt --check + clippy -D warnings + every test suite
make test         # only the tests (Rust + Go + TS), no lints
make e2e          # full pipeline (run → compare → report) per language
                  # + regression-gate (synth 30%-slower head, expect exit 2)
make act-ci       # run .github/workflows/ci.yml locally inside Docker via `act`
                  # (requires Docker running)
make install      # `cargo install` the aatxe CLI to ~/.cargo/bin
```

`make e2e` exercises three language adapters back-to-back: it builds the
example runners under `examples/`, executes them through the aatxe CLI,
compares each output against itself, and asserts the rendered markdown
carries the sticky marker. The regression-gate step synthesises a +30%
RunReport pair and pins `aatxe compare --fail-on-regression` to exit 2.

`make act-ci` runs every job from `.github/workflows/ci.yml` inside Docker
with [`act`](https://github.com/nektos/act). On Apple Silicon the workflow
runs under `--container-architecture linux/amd64` against the
`catthehacker/ubuntu:act-latest` image (the Makefile passes those flags
for you). `make act-ci-rust` runs just the heaviest job in isolation.

## Install

One-shot script-install of the latest released binary:

```sh
curl -fsSL https://raw.githubusercontent.com/enekos/aatxe/master/scripts/install.sh | sh
```

By default this drops `aatxe` at `$HOME/.local/bin/aatxe` and verifies
the asset's `sha256` before writing the file. Knobs:

```sh
# Pin a version
AATXE_VERSION=v0.2.0 curl -fsSL https://raw.githubusercontent.com/enekos/aatxe/master/scripts/install.sh | sh

# Install system-wide (needs sudo)
sudo AATXE_PREFIX=/usr/local curl -fsSL https://raw.githubusercontent.com/enekos/aatxe/master/scripts/install.sh | sh

# Skip the checksum (not recommended)
AATXE_NO_CHECK=1 curl -fsSL https://raw.githubusercontent.com/enekos/aatxe/master/scripts/install.sh | sh
```

Alternative install paths:

* **From source.** `git clone … && cargo install --path crates/aatxe`.
* **Cargo.** Once published to crates.io: `cargo install aatxe`.

Releases are tag-driven — see `.github/workflows/release.yml` for the
build matrix (darwin-arm64, darwin-x86_64, linux-x86_64, linux-aarch64).

## Quick start

### 1. Write benches in your language of choice

**TypeScript** (`sdk/ts`):

```ts
import { bench } from '@aatxe/bench'
bench('parse: phone', () => parsePhone('+34 612 345 678'))
```

**Go** (`sdk/go`):

```go
import aatxe "github.com/enekos/aatxe/sdk/go"

func main() {
    s := aatxe.NewSuite("my-svc")
    s.Bench("parse_phone", func() { _ = ParsePhone("+34 612 345 678") })
    s.EmitStdout()
}
```

**Rust** (`sdk/rust`):

```rust
use aatxe_bench::{bench, Suite};

fn main() {
    let mut suite = Suite::new("my-svc");
    bench(&mut suite, "parse_phone", || {
        let _ = parse_phone("+34 612 345 678");
    });
    suite.emit_stdout();
}
```

### 2. Run them locally

```bash
aatxe run --lang ts   --out /tmp/head.json
aatxe run --lang go   --out /tmp/head.json
aatxe run --lang rust --out /tmp/head.json
```

### 3. Compare and post the sticky comment

```bash
aatxe compare --base /tmp/base.json --head /tmp/head.json \
    --threshold 0.05 --alpha 0.05 \
    --markdown /tmp/report.md --out /tmp/cmp.json \
    --fail-on-regression

aatxe comment --report /tmp/report.md   # uses GITHUB_TOKEN + GITHUB_REPOSITORY
```

…or hand everything to the reusable workflow:

```yaml
# .github/workflows/perf.yml in your service repo
jobs:
  perf:
    uses: enekos/aatxe/.github/workflows/aatxe.yml@main
    with:
      lang: ts
      service: my-svc
      affected: true
```

## Methodology

A change is flagged **Regression** / **Improvement** when *all three* hold:

1. `|Δmedian| ≥ thresholdPct` (default 5%) — meaningful effect.
2. `p < alpha` (default 0.05) under Mann–Whitney U — statistically significant
   without any normality assumption.
3. Not noise-gated — `max(CV_base, CV_head) ≤ 25%` *or* `|Δmedian| ≥ 2 × maxCv`.

Effect size uses the **median** rather than the mean: bench distributions
have heavy right tails (GC pauses, scheduler pre-emption) that drag the
mean around. Mann–Whitney U is non-parametric, so it's robust to those
same tails.

See `crates/aatxe-core/src/stats.rs` for the full implementation. The same
algorithm is mirrored in `sdk/ts/src/stats.ts` and `sdk/go/aatxe.go` so a
producer can emit complete reports without the Rust binary in the loop.

## Workspace layout

```
aatxe/
├── crates/
│   ├── aatxe-core/         # types · stats · compare · report · affected · github (pure)
│   ├── aatxe-council/      # MoA proposer→judge LLM PR-reviewer (pure)
│   ├── aatxe-evals/        # eval harness — scores council + stats end to end
│   └── aatxe/              # CLI binary + language adapters + reqwest GH client
├── sdk/
│   ├── ts/                 # @aatxe/bench  npm package (bench API + runner)
│   ├── go/                 # aatxe Go module (Bench + Suite)
│   └── rust/               # aatxe-bench crate (Suite::bench / Suite::emit_stdout)
├── examples/
│   ├── ts-example/         # smoke bench files for each adapter
│   ├── go-example/
│   ├── rust-example/
│   └── council-bench/      # microbenches for the council pipeline
├── evals/
│   ├── council/cases/      # labeled diff fixtures + ground-truth JSON
│   └── council/baselines/  # committed baselines the CI gate diffs against
└── .github/workflows/
    ├── ci.yml                       # builds + tests the workspace (Rust + Go + TS)
    ├── aatxe.yml                    # reusable workflow for downstream services
    ├── aatxe-council.yml            # reusable council workflow
    ├── aatxe-council-selftest.yml   # council selftest (stub or real Kimi)
    └── aatxe-evals.yml              # eval harness — baseline-gated on every PR
```

## Subcommands

```
aatxe run       --lang <ts|go|rust> [--out <file>] [--service <name>] [--ref <ref>]
                [--filter <regex>] [--affected --base <ref>] [pattern...]
aatxe compare   --base <a.json> --head <b.json> [--out <cmp.json>] [--markdown <md>]
                [--threshold 0.05] [--alpha 0.05] [--noisy-cv 0.25] [--fail-on-regression]
aatxe report    --diff <cmp.json> [--out <md>]
aatxe comment   --report <md> [--repo owner/name] [--pr <num>] [--token <token>]
aatxe affected  --lang <ts|go|rust> --base <ref> [--show-all] [pattern...]
aatxe list      --lang <ts|go|rust> [pattern...]
aatxe council   [--pr <num>] [--diff-file <path>] [--model kimi-k2.6]
                [--confidence-floor 0.55] [--ignore <pat>...] [--out <json>]
                [--markdown <md>] [--post] [--fail-on-critical]
```

Exit codes: `0` success, `1` runtime error, `2` regressions detected (when
`--fail-on-regression`) **or** a `critical` council finding survived (when
`--fail-on-critical`).

## Agent council

Aatxe ships a second, optional subsystem alongside the perf gate: a
single-layer **mixture-of-agents** PR reviewer backed by [Kimi
K2.6](https://platform.moonshot.ai/), with a dedicated judge agent. It's
opt-in (it never runs unless you call `aatxe council` or include the
`aatxe-council.yml` workflow) and uses its own sticky marker
`<!-- aatxe:council -->` so the perf comment and the council comment
coexist on the same PR without colliding.

### Why this shape

Research summary (see `crates/aatxe-council/src/lib.rs` for the in-code
notes):

* **Single-layer MoA over multi-round debate.** Du et al. (2023) and a
  string of 2025 follow-ups (notably arXiv 2503.12029 on code
  summarization) show that LLM debate's gains for code tasks are
  inconsistent and triple the cost. Proposer→judge is what every
  production PR reviewer (Qodo Merge, CodeRabbit, MARS) converges to.
* **Heterogeneity from prompts, not weights.** Every proposer is the same
  Kimi model with a distinct system prompt: correctness, security,
  performance, maintainability. Wang et al.'s MoA result on
  AlpacaEval 2.0 used different open-source models; for code review,
  vendor reports (Qodo) suggest persona diversity matters more than
  weight diversity.
* **Self-review via a dedicated judge, not self-revision.** Zheng et al.
  (2023) documented a 10–25 point self-preference bias when judges grade
  their own work, so the judge is a structurally separate role with its
  own prompt that explicitly tells it to score (`keep` / `downgrade` /
  `drop` + confidence ∈ [0, 1]), never propose. Findings with judge
  confidence below `--confidence-floor` are hidden.
* **Pre-filter before the LLM ever sees the diff.** Lockfiles, vendored
  code, generated `.pb.go` / `.gen.go`, build artefacts are dropped at
  parse time. Audits of LLM PR reviewers attribute the majority of "nit
  spam" to generated-file noise; this is the single highest-ROI fix.
* **Structured JSON outputs.** Kimi's `response_format: json_object` is
  used on every call. The tolerant parser handles fence-wrapped or
  prose-prefixed outputs as a fallback.
* **Fail-soft, not fail-fast.** A 429-blown proposer doesn't abort the
  council — it surfaces in the rendered telemetry table with the error
  text, and the other three personas still produce a useful review. The
  Kimi client itself does bounded exponential backoff on 408/425/429 and
  500/502/503/504; auth + config errors (401/403/404/422) bail
  immediately. If the judge call dies, every candidate ships at the
  parser's fallback (Keep / 0.5 confidence) and the failure is flagged at
  the top of the comment.
* **Cost telemetry.** Total prompt + completion tokens are summed across
  every call and displayed inline, plus a per-agent token column in the
  collapsed telemetry table — so you can calibrate
  `--confidence-floor` and chunk policy against real spend.

### Pipeline

```
   PR diff (from GH `pulls/{n}` w/ Accept: vnd.github.v3.diff)
       │
       ▼  parse_unified_diff + filter_ignored (drops generated/lock/vendor)
       │
       ▼  chunk_for_review (greedy, ~120 KB chunks)
       │
       ▼  4 proposer agents IN PARALLEL (std::thread::scope) ──┐
       │    correctness / security / performance / maintain.   │ JSON
       │                                                       │ Finding[]
       ▼  dedup_and_rank (token-Jaccard ≥ 0.55, ±3 lines)      │
       │                                                        │
       ▼  judge agent (1 call, temperature 0.0, scores all)    │
       │    keep / downgrade / drop + confidence ∈ [0, 1]      │
       ▼  render_markdown → sticky `<!-- aatxe:council -->`    │
       ▼  GH PR comment ◄────────────────────────────────────────┘
```

### Quick start

```bash
export KIMI_API_KEY=sk-...     # from platform.moonshot.ai (or sk-kimi-... for the coding endpoint)
export GITHUB_TOKEN=...         # PAT or Actions token with `pull-requests: write`
export GITHUB_REPOSITORY=enekos/aatxe

aatxe council --pr 42 \
    --confidence-floor 0.55 \
    --fail-on-critical \
    --markdown /tmp/council.md \
    --post
```

### Backends

The council shells out to a local agent CLI per LLM call; the agent
does the model + tool-use loop and writes the final assistant message
to stdout. Two backends are wired today, selectable with `--backend`:

| `--backend`      | binary it shells out to | auth                                  | endpoint                |
|------------------|-------------------------|---------------------------------------|-------------------------|
| `pi-proxy` (default) | `pi` (One Ping agent CLI)        | `KIMI_API_KEY` env var               | Moonshot `kimi-coding`  |
| `claude-code`    | `claude` (Claude Code CLI)       | your Claude Code subscription/auth   | Anthropic               |

Both backends pass a **read-only** tool allowlist (`Read`/`Grep`/`Glob`
or equivalent) to the underlying agent. The allowlist is hardcoded in
`pi_proxy.rs`/`claude_code.rs` and cannot be widened from outside —
council can never run `Bash`, `Edit`, or `Write`.

Backend-specific environment knobs:

```bash
# pi-proxy
PI_BIN=/custom/path/to/pi PI_MODEL=kimi-k2-thinking aatxe council --pr 42

# claude-code
CLAUDE_BIN=/custom/path/to/claude CLAUDE_MODEL=opus \
    CLAUDE_MAX_BUDGET_USD=2.0 \
    aatxe council --pr 42 --backend claude-code
```

### Streaming pipeline events

For long-running runs (real-LLM calls are minutes per proposer × four
proposers per chunk), pass `--json-events <path>` to emit a JSON-Lines
log of pipeline events:

```bash
aatxe council --pr 42 --json-events /tmp/council.events.jsonl
# tail it from another terminal:
tail -f /tmp/council.events.jsonl | jq -c 'select(.kind=="proposer_done")'
```

Use `--json-events -` to stream to stdout. The event taxonomy is
`start`, `proposer_start`, `proposer_done`, `synthesize_done`,
`judge_start`, `judge_done`, `finding_emitted`, `done` — see
`crates/aatxe-council/src/events.rs` for the full schema.

### Interactive curation

By default (when stdin is a TTY *and* `--post` is set), the council
pauses after the judge stage and walks the user through every
shippable finding for a keep/drop decision. Force-on/force-off with
`--interactive=true` / `--interactive=false`:

```
[1/3] CRITICAL [security] src/admin.ts:23
  IDOR: /users/:id/export discloses any user's data
  Rationale: handler queries users by req.params.id without checking req.user.id matches.
  Confidence: 0.91
  [k]eep / [d]rop / [s]kip-all / [q]uit-all (default k): d

[2/3] MAJOR [correctness] src/db.ts:14
  …
```

Dropped findings have their judge verdict flipped to `drop`, so the
rendered markdown body filters them out via the existing
`shippable()` path — the comment posted to GitHub matches what the
human saw after curation.

### Pre-PR self-review

Run the council against your working tree's diff against `origin/master`
before opening the PR:

```bash
make council-self            # uses the configured backend (real LLM calls)
make council-self-stub       # same flow, deterministic stub (no quota)

# or by hand against any base ref:
BASE_REF=origin/main make council-self
aatxe council --diff-file <(git diff origin/master...HEAD)
```

The `make` targets stage `tmp/council-self.{diff,json,md}` so you can
inspect the artefacts before re-running. Combine with `--interactive`
(default-on for TTY use) and `--confidence-floor 0.65` for a tight
self-review loop:

```bash
aatxe council \
    --diff-file <(git diff origin/master...HEAD) \
    --confidence-floor 0.65 \
    --interactive
```

### Confidence-floor calibration

`make evals-calibrate` sweeps the eval corpus at multiple
`--confidence-floor` values and prints a side-by-side metric table,
making the choice of floor data-justified instead of a guess:

```bash
AATXE_FLOORS="0.55 0.60 0.65" make evals-calibrate
# floor=0.55 → tmp/calibrate/floor-0.55.json
# floor=0.60 → tmp/calibrate/floor-0.60.json
# floor=0.65 → tmp/calibrate/floor-0.65.json
#
# ## floor=0.65 vs floor=0.55
#   metric                               baseline         head            Δ
#   …
#   avgFalsePositivesPerCase                4.400        2.600       -1.800
```

Real-LLM calibration is gated behind `USE_REAL_KIMI=true` (it takes
~60 min per floor) — use it when promoting a floor that the stub
sweep proves is worth measuring against the real backend.

### Stub mode (offline / CI smoke test)

Setting `AATXE_COUNCIL_STUB=1` bypasses Moonshot entirely and uses a
deterministic canned-response stub keyed to the bundled fixture diff.
The workflow `aatxe-council-selftest.yml` runs this under `act` so we
can verify the whole plumbing (workspace build → CLI run → sticky body
→ JSON shape) without burning quota. `make act-council` is the
shortcut; set `USE_REAL_KIMI=true KIMI_API_KEY=...` to flip it to real
calls.

…or via the reusable workflow:

```yaml
# .github/workflows/review.yml in your service repo
jobs:
  council:
    uses: enekos/aatxe/.github/workflows/aatxe-council.yml@main
    with:
      confidence-floor: '0.55'
      fail-on-critical: true
    secrets:
      KIMI_API_KEY: ${{ secrets.KIMI_API_KEY }}
    permissions:
      pull-requests: write
      contents: read
```

### Bench coverage

The council's pure pipeline is benched with `aatxe-bench` itself —
the same stats engine that judges aatxe's own perf regressions also
judges the council's. See `examples/council-bench/`. The suite
(`make council-bench`) covers diff parsing, path filtering, chunking,
prompt assembly, JSON response parsing, deterministic synthesis, and the
end-to-end `run_council` orchestration against a stub LLM. `make
council-bench-self` proves the perf gate would catch a regression in the
council itself by comparing the bench output against its own baseline.

### Environment

| Var | Required | Default | Purpose |
| --- | --- | --- | --- |
| `KIMI_API_KEY` | yes | — | Moonshot API key. `sk-kimi-...` switches to the coding endpoint automatically. |
| `KIMI_BASE_URL` | no | `https://api.moonshot.ai/v1` | Override (e.g. self-hosted Kimi). |
| `KIMI_MODEL` | no | `kimi-k2.6` | Override the council model. |
| `GITHUB_TOKEN` / `GH_TOKEN` | yes | — | PAT or Actions token with `pull-requests: write`. |
| `GITHUB_REPOSITORY` | yes | — | `owner/name` for the PR. |
| `AATXE_PR` / `GITHUB_REF` | (auto) | — | PR number; auto-detected on Actions. |

## Evals

Unit tests prove individual functions are correct. Evals prove the whole
pipeline works end to end on representative inputs — same shape as
swe-bench for the council half and ROC-style synthetic ground truth for
the stats half.

```bash
make evals          # stub LLM, baseline-gated, deterministic, ~2s
make evals-real     # real Kimi, no gate. Requires KIMI_API_KEY.
make evals-update-baseline   # promote the current stub run as the new baseline
```

The harness has two surfaces:

* **Council quality** — labeled PR diff fixtures under
  [`evals/council/cases/`](evals/council/cases/), each carrying expected
  findings (`mustCatch=true` lines the council should surface),
  bonus findings, and forbidden paths the pre-filter must drop. The
  scorer reports per-severity recall, critical-finding precision,
  severity-calibration MAE, judge-confidence Brier score, false-positive
  count per case, and forbidden-path findings. The corpus ships 15
  cases: 10 small synthetic ones (security password log, SSRF, null
  deref, off-by-one, unwrap-in-handler, N+1, TODO doc, clean PRs,
  generated-code-only) plus 5 real-world-shaped cases with multi-file
  diffs, full post-PR file fixtures, and — for three of them —
  related-file context that exercises project-wide review:
  `perf-django-export-n-plus-one` (Django export endpoint; 2 of the 5
  must-catch findings only fire when the model sees the
  `prefetch_in_batches` and `BillingClient.batch_charge` helpers shipped
  as related context in `app/utils/`),
  `security-authz-idor-export-route` (Express IDOR plus
  password-hash/2FA-secret exfiltration; the convention-violation catch
  requires reading the `requireOwner` helper in
  `src/middleware/authz.ts`) and `maintainability-rust-reinvents-counters`
  (axum upload handler rolls its own `Arc<Mutex<HashMap<String, u64>>>`
  counter when the repo's canonical `Counters` type — visible as
  related context in `src/metrics/mod.rs` — is the documented pattern).
  The other two real-world cases (`security-jwt-fallback-secret`,
  `correctness-cache-race-stale-ttl`) test file-level context utility —
  findings catchable only with the surrounding imports or a
  file-header invariant the hunk alone wouldn't reveal.
* **Stats engine** — synthetic A/B benchmark pairs with known ground
  truth (null, clear-regression, borderline-regression, clear-improvement,
  noise-swamps-signal, below-threshold). Each scenario runs 200 trials
  with deterministic SplitMix64 seeds; the scorer reports
  regression/improvement/neutral rates per scenario, mean p-value, and
  whether the configured expectations held.

### CI gate

A baseline JSON lives at
[`evals/council/baselines/stub.json`](evals/council/baselines/stub.json).
`aatxe evals --baseline …` diffs the current run against it and exits
2 if any headline metric regressed past its tolerance — same exit-code
contract as `aatxe compare --fail-on-regression`. The reusable workflow
[`.github/workflows/aatxe-evals.yml`](.github/workflows/aatxe-evals.yml)
runs on every PR in stub mode and uploads the JSON + markdown summary
as an artefact. To measure real-Kimi quality, dispatch the workflow
manually with `use-real-kimi=true` (requires a `KIMI_API_KEY` repo
secret).

### What the metrics mean

| metric | meaning | direction |
|---|---|---|
| `critical_recall` | fraction of `must_catch=true critical` labels covered by a shippable finding | higher = better |
| `critical_precision` | fraction of shippable critical findings that landed on a labeled finding | higher = better |
| `severity_calibration_mae` | mean abs distance (rungs, 0–3) between model severity and label severity on matched findings | lower = better |
| `judge_brier_score` | mean `(judge_confidence − outcome)²` over shippable findings; 0 = perfect, 0.25 = chance | lower = better |
| `avg_false_positives_per_case` | mean unmatched-shippable + forbidden-path findings per case | lower = better |
| `forbidden_path_findings` | shippable findings on lockfiles / generated code | always 0 |
| `observed_null_fpr` | fraction of null-distribution trials the stats gate fired on | target ≤ α (0.05) |
| `observed_borderline_tpr` | fraction of 6% true-regression trials the stats gate caught | target ≥ 0.55 |

### Adding a case

A minimal case is two files under `evals/council/cases/`:

```text
evals/council/cases/correctness-mutex-deadlock.diff   # the unified-diff fixture
evals/council/cases/correctness-mutex-deadlock.json   # the ground truth
```

Append the JSON to `_index.json` and re-run `make evals-update-baseline`.

#### File-context cases (project-wide reviewing)

The interesting bugs in real code only surface when the reviewer can
read the file the hunk is in — see a function's full body, the file
header invariant, the surrounding imports. The council learns this
context via the `filesDir` field on a case:

```text
evals/council/cases/security-jwt-fallback-secret.diff
evals/council/cases/security-jwt-fallback-secret.json     # has filesDir: "files/security-jwt-fallback-secret"
evals/council/cases/files/security-jwt-fallback-secret/
    src/auth/jwt.ts          # full post-PR contents of every file the diff touches
    src/routes/auth.ts
    src/config/env.ts
```

The harness walks `filesDir` recursively, treats every path inside as
repo-rooted (so `files/<case>/src/auth/jwt.ts` becomes context for the
diff path `src/auth/jwt.ts`), and attaches each file's contents to the
matching `ParsedFile.context` slot before any LLM call. Proposers then
see a new section in the user message:

```text
File contents (post-PR):

=== src/auth/jwt.ts ===
```rust
... full file ...
```

Unified diff:
```diff
... hunks ...
```
```

Budgets for the new section live on `ChunkPolicy` (defaults: 64 KB per
file, 256 KB per chunk); oversized context is truncated middle-out with
a `[truncated]` marker, and context past the chunk budget is dropped
silently (diff is never dropped). Files in the diff that aren't in
`filesDir` review diff-only — the feature is purely opt-in per file.

##### Related-file context (cross-reference helpers, conventions, patterns)

A case's `filesDir` can also carry files that **aren't in the diff** —
helpers, existing patterns, header docs the diff *references* but doesn't
modify. The harness classifies anything in `filesDir` that doesn't match
a diff path as **related context** and the pipeline packs it into every
chunk produced from that diff. Proposers see a third section:

```text
Files in this chunk:
- app/views/exports.py  (+15 / -0)  (+context)

Related repository files (NOT in this diff — read-only cross-reference):
- app/utils/billing.py (1042 bytes)
- app/utils/db.py (724 bytes)

File contents (post-PR):
=== app/views/exports.py === ...

Related repository context (not in diff):
=== app/utils/billing.py === ...
=== app/utils/db.py === ...

Unified diff: ...
```

The system prompt explicitly tells the model that related files are
read-only ("do NOT raise findings against unchanged lines in them") so
the false-positive surface stays bounded; case authors can also add a
`forbidden:` entry pointing at the related file path as belt-and-braces,
and the scorer will flag any finding that lands there as a false
positive.

Why this matters: the strongest signal a reviewer can give is "use the
existing helper", which is unreachable without related context.
`perf-django-export-n-plus-one` ships `app/utils/db.py` (exposes
`prefetch_in_batches` and `exists_fast`) and `app/utils/billing.py`
(exposes `BillingClient.batch_charge` with a docstring that mandates
batched calls); two of its five must-catch findings only fire when the
model can read those helpers. `security-authz-idor-export-route` ships
`src/middleware/authz.ts` whose top-doc cites two prior IDOR incidents
as the justification for the convention the diff is silently breaking.
`maintainability-rust-reinvents-counters` ships `src/metrics/mod.rs`
whose module doc explicitly discourages the `Mutex<HashMap>` counter
pattern the diff introduces.

Related-context budgets are independent of per-file budgets and live on
`ChunkPolicy` too (defaults: 32 KB per related file, 128 KB per chunk).
Related files past the chunk budget are dropped silently in
declaration order; per-file diffs and per-file context always survive.

##### Building a case

Cases authored with line-perfect alignment are easiest to build by
writing a `before/` and `after/` tree under `/tmp/<scratch>/`, running
`diff -u` to produce the hunks, and copying the `after/` tree into
`evals/council/cases/files/<case>/`. To exercise related context,
include files in the `after/` tree that the diff doesn't touch — they
end up in the fixtures dir and the harness routes them to the
related-context slot automatically. The five real-world cases bundled
today were all authored this way.

The case JSON shape — including `filesDir`, `expected[]`, `forbidden[]`,
and `maxFindings` — is documented in
[`crates/aatxe-evals/src/council.rs`](crates/aatxe-evals/src/council.rs)
(`CouncilCase`).

##### End-to-end prompt-shape tests

Four integration tests in
[`crates/aatxe/tests/eval_cases_integration.rs`](crates/aatxe/tests/eval_cases_integration.rs)
load real cases from disk and assert the actual proposer prompts carry
the right file-context and related-context blocks — useful as a
regression net for the prompt builder + harness wiring.

## Learning corpus (`aatxe learn`)

The council gets *better* over time on a given repo because it ingests
human feedback on prior PRs and folds that into a tiny, bounded,
self-healing JSON corpus persisted between runs as a GitHub Actions
artifact (`aatxe-learning-corpus`). The corpus is then injected as a
project-specific guidance block into the proposer + judge system prompts,
so the model sees "what *this* project's humans have endorsed or refuted"
before reviewing a new diff.

### What gets persisted

Only the *highest-signal* feedback. In priority order:

1. **`aatxe: remember <…>`** in any PR comment — the highest-authority
   signal a human can give. Lands as a `UserDirective` entry.
2. **`aatxe: good catch on N`** / **`aatxe: false-positive on N`** —
   confirm/refute a specific shipped finding by 0-based index (matching
   the rendered council comment).
3. **Reactions on the council sticky comment** — 👍/❤️/🚀/🎉 confirm,
   👎/😕 refute. Coarse (we don't know which finding was reacted to) so
   weighted against the top-severity shipped finding.

Lower-signal signals (inferred merge outcomes) are reserved fields for
later; the loader already accepts them.

Directive lines must start with `aatxe:` (or `@aatxe`) after whitespace
— mid-paragraph mentions are intentionally ignored so the parser can't
be fooled by reviewers quoting the docs.

### How it stays bounded and clean

* **Score function** — `source_authority + confirmations − 2 ×
  refutations`, multiplied by an exponential 60-day recency decay.
  Refutations cost twice what confirmations earn — the corpus biases
  precision over recall, because confidently asserting the wrong thing
  pollutes every future review.
* **Compaction** — every harvest cycle truncates to the keep-best-N
  (default 100) and drops entries below `min_score` (default 0.1).
* **Self-healing load** — `load_self_healing` always returns a usable
  corpus. Missing file → empty. Malformed top-level JSON → empty +
  `corpus_was_invalid: true` in the summary. Malformed individual
  entries → those entries dropped, the rest survives, count surfaced.
  Future schema versions → empty + `corpus_from_future_version: Some(v)`
  so the workflow can warn rather than silently downgrade.

### CLI surface

```bash
# Pull the PR's comments + reactions, harvest signals, merge into the
# corpus, compact, and write back. Defaults to aatxe-learning-corpus.json.
aatxe learn harvest \
    --corpus .aatxe/aatxe-learning-corpus.json \
    --pr 123 \
    --council-report .aatxe/aatxe-council.json

# Recompute scores + truncate. Idempotent.
aatxe learn compact --corpus .aatxe/aatxe-learning-corpus.json

# Print the current corpus on disk.
aatxe learn show --corpus .aatxe/aatxe-learning-corpus.json
```

The council picks up the corpus with `--learning-corpus <path>`:

```bash
aatxe council \
    --pr 123 \
    --learning-corpus .aatxe/aatxe-learning-corpus.json \
    --post
```

### CI wiring

`.github/workflows/aatxe-learn.yml` is a reusable workflow downstream
services call to get the full loop:

```yaml
jobs:
  review:
    uses: enekos/aatxe/.github/workflows/aatxe-learn.yml@main
    with:
      fail-on-critical: true
    secrets:
      KIMI_API_KEY: ${{ secrets.KIMI_API_KEY }}
```

Steps the workflow runs:

1. Download the previous `aatxe-learning-corpus` artifact (no-op if none).
2. Run `aatxe council --learning-corpus …` with corpus injection. Posts
   the sticky comment.
3. Run `aatxe learn harvest …` to pull reactions + comments from this
   PR and merge them into the corpus.
4. Re-upload the updated corpus as the new `aatxe-learning-corpus`
   artifact, retention 90 days.

### Local iteration

```bash
make learn-seed     # synthesise a PR's worth of feedback, harvest, show
make learn-show     # print the corpus on disk
make learn-compact  # rescore + truncate, idempotent
```

## License

MIT. See [`LICENSE`](LICENSE).
