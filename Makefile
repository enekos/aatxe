# Aatxe — top-level driver.
#
# `make help` is the entrypoint. Sane defaults: targets do what their name
# implies, are idempotent, and print one line of progress per stage.
#
# Conventions:
#   - Anything that builds a binary lands in `target/release/` (Cargo) or
#     `sdk/ts/dist/` (TS) or `bin/` (helpers we generate).
#   - Anything that produces test artifacts lands in `tmp/` — gitignored.
#   - The `e2e-*` targets exercise the whole pipeline (run → compare →
#     report) end-to-end against the bundled examples.
#   - The `act-*` targets run our GitHub Actions workflows locally via `act`.

CARGO       ?= cargo
GO          ?= go
NODE        ?= node
NPM         ?= npm
ACT         ?= act

REPO_ROOT   := $(shell pwd)
TMP         := $(REPO_ROOT)/tmp
AATXE_BIN   := $(REPO_ROOT)/target/release/aatxe
RUST_RUNNER := $(REPO_ROOT)/target/release/aatxe-rust-runner

# Pretty-printer for stage headers.
define say
	@printf '\033[1;34m▶ %s\033[0m\n' "$(1)"
endef

.PHONY: help
help: ## Print every target with its description
	@awk 'BEGIN {FS=":.*##"; printf "Available targets:\n"} \
	     /^[a-zA-Z_-]+:.*?##/ { printf "  \033[1;36m%-22s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

# ----------------------------------------------------------------------------
# Build
# ----------------------------------------------------------------------------

.PHONY: build
build: build-rust build-ts build-go ## Build every artifact (Rust + TS dist + Go vet)

.PHONY: build-rust
build-rust: ## Build the Rust workspace in release mode (CLI + SDK + example runner)
	$(call say,build-rust)
	$(CARGO) build --release --workspace

.PHONY: build-ts
build-ts: ## Type-check + emit dist/ for the TS SDK
	$(call say,build-ts)
	cd sdk/ts && [ -d node_modules ] || $(NPM) install --silent
	cd sdk/ts && $(NPM) run --silent build

.PHONY: build-go
build-go: ## go vet the Go SDK (build is implicit in `go test`)
	$(call say,build-go)
	cd sdk/go && $(GO) vet ./...

# ----------------------------------------------------------------------------
# Quality gates
# ----------------------------------------------------------------------------

.PHONY: test
test: test-rust test-go test-ts ## Run every test suite

.PHONY: test-rust
test-rust: ## Run the Rust unit + integration tests across the workspace
	$(call say,test-rust)
	$(CARGO) test --workspace --locked

.PHONY: test-go
test-go: ## Run the Go SDK tests with -race
	$(call say,test-go)
	cd sdk/go && $(GO) test -race ./...

.PHONY: test-ts
test-ts: ## Run the TS SDK unit tests via node --test
	$(call say,test-ts)
	cd sdk/ts && [ -d node_modules ] || $(NPM) install --silent
	cd sdk/ts && $(NPM) test --silent

.PHONY: fmt
fmt: ## Apply cargo fmt
	$(call say,fmt)
	$(CARGO) fmt --all

.PHONY: lint
lint: ## cargo fmt --check + clippy -D warnings + go vet
	$(call say,lint)
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	cd sdk/go && $(GO) vet ./...

.PHONY: check
check: lint test ## Lint + every test suite. CI uses this.

# ----------------------------------------------------------------------------
# End-to-end pipeline (against bundled examples)
# ----------------------------------------------------------------------------

$(TMP):
	@mkdir -p $(TMP)

.PHONY: e2e
e2e: e2e-rust e2e-go e2e-ts e2e-regression-gate council-bench-self core-bench-self ast-bench-self evals ## Run the full aatxe pipeline against every adapter + a fake regression + self-bench compares (council/core/ast) + the stub-LLM eval harness

.PHONY: e2e-rust
e2e-rust: $(TMP) build-rust ## Rust example: run → compare against itself → markdown
	$(call say,e2e-rust)
	AATXE_RUST_RUNNER="$(RUST_RUNNER)" \
	    $(AATXE_BIN) run --lang rust --service example-rust \
	        --cwd $(REPO_ROOT)/examples/rust-example \
	        --out $(TMP)/rust.json
	$(AATXE_BIN) compare --base $(TMP)/rust.json --head $(TMP)/rust.json \
	    --out $(TMP)/rust.cmp.json --markdown $(TMP)/rust.md
	@grep -q '<!-- aatxe:report -->' $(TMP)/rust.md \
	    && echo "    ✓ rust e2e: sticky marker present" \
	    || (echo "    ✗ rust e2e: sticky marker missing"; exit 1)

.PHONY: e2e-go
e2e-go: $(TMP) build-rust ## Go example: build runner, run → compare → markdown
	$(call say,e2e-go)
	cd examples/go-example && $(GO) build -o $(TMP)/aatxe-go-runner .
	AATXE_GO_RUNNER="$(TMP)/aatxe-go-runner" \
	    $(AATXE_BIN) run --lang go --service example-go \
	        --cwd $(REPO_ROOT)/examples/go-example \
	        --out $(TMP)/go.json
	$(AATXE_BIN) compare --base $(TMP)/go.json --head $(TMP)/go.json \
	    --out $(TMP)/go.cmp.json --markdown $(TMP)/go.md
	@grep -q '<!-- aatxe:report -->' $(TMP)/go.md \
	    && echo "    ✓ go e2e: sticky marker present" \
	    || (echo "    ✗ go e2e: sticky marker missing"; exit 1)

.PHONY: e2e-ts
e2e-ts: $(TMP) build-rust build-ts ## TS example: run via the TS runner → compare → markdown
	$(call say,e2e-ts)
	cd examples/ts-example && [ -d node_modules ] || $(NPM) install --silent
	# Drive the runner directly: examples/ts-example sits next to the dist
	# output, so `node ../../sdk/ts/dist/runner.js` is our adapter entrypoint.
	AATXE_TS_RUNNER="$(NODE) $(REPO_ROOT)/sdk/ts/dist/runner.js" \
	    $(AATXE_BIN) run --lang ts --service example-ts \
	        --cwd $(REPO_ROOT)/examples/ts-example \
	        --out $(TMP)/ts.json
	$(AATXE_BIN) compare --base $(TMP)/ts.json --head $(TMP)/ts.json \
	    --out $(TMP)/ts.cmp.json --markdown $(TMP)/ts.md
	@grep -q '<!-- aatxe:report -->' $(TMP)/ts.md \
	    && echo "    ✓ ts e2e: sticky marker present" \
	    || (echo "    ✗ ts e2e: sticky marker missing"; exit 1)

.PHONY: e2e-regression-gate
e2e-regression-gate: $(TMP) build-rust ## Synthesise a 30%-slower head and confirm `aatxe compare --fail-on-regression` exits 2
	$(call say,e2e-regression-gate)
	@scripts/e2e-regression-gate.sh $(AATXE_BIN) $(TMP)

# ----------------------------------------------------------------------------
# Agent council (LLM-backed, Kimi)
# ----------------------------------------------------------------------------

COUNCIL_BENCH_BIN := $(REPO_ROOT)/target/release/aatxe-council-bench

.PHONY: council-bench
council-bench: $(TMP) build-rust ## Run the council pure-logic benches and dump the RunReport
	$(call say,council-bench)
	$(CARGO) build --release --bin aatxe-council-bench
	AATXE_SERVICE=aatxe-council $(COUNCIL_BENCH_BIN) > $(TMP)/council.json
	@head -c 600 $(TMP)/council.json && echo "" && echo "    ✓ council-bench: wrote $(TMP)/council.json"

.PHONY: council-bench-self
council-bench-self: council-bench ## Compare the council bench against itself — proves the regression gate works on council code
	$(call say,council-bench-self)
	$(AATXE_BIN) compare --base $(TMP)/council.json --head $(TMP)/council.json \
	    --out $(TMP)/council.cmp.json --markdown $(TMP)/council.md
	@grep -q '<!-- aatxe:report -->' $(TMP)/council.md \
	    && echo "    ✓ council-bench-self: sticky marker present" \
	    || (echo "    ✗ council-bench-self: sticky marker missing"; exit 1)

# ----------------------------------------------------------------------------
# Self-benches — aatxe benching its own hot code (very meta)
# ----------------------------------------------------------------------------
#
# `core-bench` covers the statistical brain (`aatxe-core`): summarize_samples,
# Mann-Whitney U, MAD, Welch-t, compare_reports, and the affected-set import
# extractor — the no-network code that runs on every gate. `ast-bench` covers
# `aatxe-ast` tree-sitter parsing — the heaviest per-call CPU on the council's
# pre-LLM path. Both feed the self-bench CI workflow (`aatxe-self-bench.yml`).

CORE_BENCH_BIN := $(REPO_ROOT)/target/release/aatxe-core-bench
AST_BENCH_BIN  := $(REPO_ROOT)/target/release/aatxe-ast-bench

.PHONY: core-bench
core-bench: $(TMP) build-rust ## Run the aatxe-core stats/compare benches and dump the RunReport
	$(call say,core-bench)
	$(CARGO) build --release --bin aatxe-core-bench
	AATXE_SERVICE=aatxe-core $(CORE_BENCH_BIN) > $(TMP)/core.json
	@head -c 400 $(TMP)/core.json && echo "" && echo "    ✓ core-bench: wrote $(TMP)/core.json"

.PHONY: core-bench-self
core-bench-self: core-bench ## Compare the core bench against itself — proves the gate works on aatxe-core
	$(call say,core-bench-self)
	$(AATXE_BIN) compare --base $(TMP)/core.json --head $(TMP)/core.json \
	    --out $(TMP)/core.cmp.json --markdown $(TMP)/core.md
	@grep -q '<!-- aatxe:report -->' $(TMP)/core.md \
	    && echo "    ✓ core-bench-self: sticky marker present" \
	    || (echo "    ✗ core-bench-self: sticky marker missing"; exit 1)

.PHONY: ast-bench
ast-bench: $(TMP) build-rust ## Run the aatxe-ast tree-sitter parse benches and dump the RunReport
	$(call say,ast-bench)
	$(CARGO) build --release --bin aatxe-ast-bench
	AATXE_SERVICE=aatxe-ast $(AST_BENCH_BIN) > $(TMP)/ast.json
	@head -c 400 $(TMP)/ast.json && echo "" && echo "    ✓ ast-bench: wrote $(TMP)/ast.json"

.PHONY: ast-bench-self
ast-bench-self: ast-bench ## Compare the ast bench against itself — proves the gate works on aatxe-ast
	$(call say,ast-bench-self)
	$(AATXE_BIN) compare --base $(TMP)/ast.json --head $(TMP)/ast.json \
	    --out $(TMP)/ast.cmp.json --markdown $(TMP)/ast.md
	@grep -q '<!-- aatxe:report -->' $(TMP)/ast.md \
	    && echo "    ✓ ast-bench-self: sticky marker present" \
	    || (echo "    ✗ ast-bench-self: sticky marker missing"; exit 1)

# ----------------------------------------------------------------------------
# Local perf-vs workflow
# ----------------------------------------------------------------------------
#
# `aatxe perf-vs` materializes a sibling worktree at a target ref, runs
# the chosen bench in both HEAD and that worktree, and pipes both
# `RunReport`s through the same comparator the CI gate uses. The
# worktree lives at `../aatxe-worktrees/<sha-short>` by default and is
# reused between runs so the cargo cache stays warm.
#
# Knobs (all optional):
#   REF       — ref to compare HEAD against. Default `origin/master`.
#   BENCH     — `council` (default, ~5s) / `big-diff` (~30s) / `core` /
#               `ast` / `all`.
#   VERBOSE   — set to anything to stream cargo + bench output.

REF   ?= origin/master
BENCH ?= council

.PHONY: perf-vs
perf-vs: $(TMP) build-rust ## Local A/B perf-compare HEAD vs $(REF) via a sibling worktree. Knobs: REF, BENCH, VERBOSE.
	$(call say,perf-vs)
	$(AATXE_BIN) perf-vs --against $(REF) --bench $(BENCH) \
	    --out-dir $(TMP)/perf-vs/$(BENCH) \
	    $(if $(VERBOSE),--verbose,)
	@echo "    ✓ perf-vs: see $(TMP)/perf-vs/$(BENCH)/{head,base}.json + cmp.{json,md}"

.PHONY: perf-vs-self
perf-vs-self: $(TMP) build-rust ## Smoke test: perf-vs against HEAD~1 (or origin/master if no prior commit). Exercises the worktree + compare path end-to-end.
	$(call say,perf-vs-self)
	@ref=$$(git rev-parse --verify --quiet HEAD~1 || git rev-parse --verify origin/master); \
	   $(AATXE_BIN) perf-vs --against $$ref --bench council \
	       --out-dir $(TMP)/perf-vs/self
	@echo "    ✓ perf-vs-self: see $(TMP)/perf-vs/self/cmp.md"

# ----------------------------------------------------------------------------
# Local realtime dashboard
# ----------------------------------------------------------------------------
#
# `aatxe ui` serves the embedded dashboard: spawn coding agents in
# isolated worktrees, re-bench on every working-tree change, stream
# trajectories + council verdicts + tournament standings to the browser.
#
# Knobs (all optional):
#   UI_PORT   — port to bind. Default 4866.
#   UI_FLAGS  — extra flags, e.g. `--stub-agent --council stub --no-open`.

UI_PORT ?= 4866

.PHONY: ui
ui: build-rust ## Launch the local realtime dashboard on $(UI_PORT). Knobs: UI_PORT, UI_FLAGS.
	$(call say,ui)
	$(AATXE_BIN) ui --port $(UI_PORT) $(UI_FLAGS)

.PHONY: ui-demo
ui-demo: build-rust ## Dashboard demo: offline stub agent + stub council, no LLM spend. Spawn an agent from the browser to watch the full loop.
	$(call say,ui-demo)
	$(AATXE_BIN) ui --port $(UI_PORT) --stub-agent --council stub

.PHONY: council-dry-run
council-dry-run: $(TMP) build-rust ## Pipe the bundled council fixture diff through `aatxe council`. Requires KIMI_API_KEY.
	$(call say,council-dry-run)
	@if [ -z "$$KIMI_API_KEY" ]; then \
	    echo "    ✗ KIMI_API_KEY is unset. Export it (or source from mairu .env) and rerun."; \
	    exit 1; \
	fi
	$(AATXE_BIN) council --diff-file examples/council-bench/fixtures/sample.diff \
	    --out $(TMP)/council-dry.json \
	    --markdown $(TMP)/council-dry.md
	@head -c 400 $(TMP)/council-dry.md && echo "" \
	    && echo "    ✓ council-dry-run: wrote $(TMP)/council-dry.{json,md}"

# ----------------------------------------------------------------------------
# Pre-PR self-review
# ----------------------------------------------------------------------------
#
# Run the council against the local working tree's diff against
# `origin/master` (or `$BASE_REF`). Useful as the last step before
# `gh pr create`. Two flavours:
#
#   • `make council-self`       — uses the user's configured backend
#                                  (pi-proxy by default). Real LLM calls.
#   • `make council-self-stub`  — same flow, but `AATXE_COUNCIL_STUB=1`
#                                  so it's fast/free. Smoke-tests the
#                                  diff-from-worktree plumbing without
#                                  burning model quota.
#
# Both write to `$(TMP)/council-self.{json,md}` and print the rendered
# markdown to stdout.

BASE_REF ?= origin/master

.PHONY: council-self
council-self: $(TMP) build-rust ## Run the council against the local diff (HEAD vs origin/master). Set BASE_REF to override.
	$(call say,council-self)
	@if ! git rev-parse --verify --quiet $(BASE_REF) > /dev/null; then \
	    echo "    ✗ base ref $(BASE_REF) not found locally — try \`git fetch origin master\` first."; \
	    exit 1; \
	fi
	@git diff $(BASE_REF)...HEAD > $(TMP)/council-self.diff
	@if [ ! -s $(TMP)/council-self.diff ]; then \
	    echo "    ✗ empty diff vs $(BASE_REF); commit something first or check the base ref."; \
	    exit 1; \
	fi
	$(AATXE_BIN) council --diff-file $(TMP)/council-self.diff \
	    --out $(TMP)/council-self.json \
	    --markdown $(TMP)/council-self.md
	@head -c 600 $(TMP)/council-self.md && echo "" \
	    && echo "    ✓ council-self: wrote $(TMP)/council-self.{diff,json,md}"

.PHONY: council-self-stub
council-self-stub: $(TMP) build-rust ## Same as council-self but with the deterministic stub (no Kimi/Claude calls).
	$(call say,council-self-stub)
	@if ! git rev-parse --verify --quiet $(BASE_REF) > /dev/null; then \
	    echo "    ✗ base ref $(BASE_REF) not found locally — try \`git fetch origin master\` first."; \
	    exit 1; \
	fi
	@git diff $(BASE_REF)...HEAD > $(TMP)/council-self.diff
	@if [ ! -s $(TMP)/council-self.diff ]; then \
	    echo "    ✗ empty diff vs $(BASE_REF); commit something first."; \
	    exit 1; \
	fi
	AATXE_COUNCIL_STUB=1 $(AATXE_BIN) council --diff-file $(TMP)/council-self.diff \
	    --out $(TMP)/council-self.json \
	    --markdown $(TMP)/council-self.md
	@head -c 400 $(TMP)/council-self.md && echo "" \
	    && echo "    ✓ council-self-stub: wrote $(TMP)/council-self.{diff,json,md}"

# ----------------------------------------------------------------------------
# Confidence-floor calibration
# ----------------------------------------------------------------------------
#
# Sweep the council's `--confidence-floor` setting against the labeled
# eval corpus and report the FP/case + critical-recall side-by-side per
# floor. Backed by `scripts/calibrate-confidence-floor.sh`.
#
# Headline goal: justify raising the floor 0.55 → 0.65 with data
# (per projects/aatxe.md:203 action #2). Without scaffolding this is a
# manual ~60-minute exercise; with it, one `make evals-calibrate` call.

.PHONY: evals-calibrate
evals-calibrate: $(TMP) build-rust ## Re-run the eval corpus at multiple --confidence-floor settings + diff metrics (stub LLM)
	$(call say,evals-calibrate)
	@scripts/calibrate-confidence-floor.sh "$(TMP)" "$(AATXE_BIN)"

.PHONY: evals-calibrate-real
evals-calibrate-real: $(TMP) build-rust ## Confidence-floor sweep against real Kimi. Requires KIMI_API_KEY. ~60min/floor.
	$(call say,evals-calibrate-real)
	@USE_REAL_KIMI=true scripts/calibrate-confidence-floor.sh "$(TMP)" "$(AATXE_BIN)"

# ----------------------------------------------------------------------------
# Learning corpus (`aatxe learn`)
# ----------------------------------------------------------------------------
#
# Self-healing learning corpus persisted as a GitHub Actions artifact
# between council runs. Three local-iteration targets:
#
#   • `make learn-seed`     — synthesise a fixture corpus + PR feedback,
#                             run a harvest cycle against them, show the
#                             resulting corpus. No network, no Kimi.
#   • `make learn-show`     — pretty-print the current corpus on disk.
#   • `make learn-compact`  — rescore + truncate + drop below-threshold
#                             entries. Idempotent.
#
# All three write to `$(TMP)/aatxe-learning-corpus.json` so the e2e cycle
# stays gitignored.

LEARN_CORPUS := $(TMP)/aatxe-learning-corpus.json

.PHONY: learn-seed
learn-seed: $(TMP) build-rust ## Seed a corpus from a synthetic PR — proves harvest+compact end-to-end with no Kimi calls
	$(call say,learn-seed)
	@scripts/learn-seed-fixture.sh "$(AATXE_BIN)" "$(LEARN_CORPUS)"
	@$(AATXE_BIN) learn show --corpus $(LEARN_CORPUS)
	@echo "    ✓ learn-seed: corpus at $(LEARN_CORPUS)"

.PHONY: learn-show
learn-show: build-rust ## Print the current corpus on disk (defaults to $(LEARN_CORPUS))
	$(AATXE_BIN) learn show --corpus $(LEARN_CORPUS)

.PHONY: learn-compact
learn-compact: build-rust ## Rescore + truncate the corpus
	$(AATXE_BIN) learn compact --corpus $(LEARN_CORPUS)

# ----------------------------------------------------------------------------
# Eval harness — `aatxe evals`
# ----------------------------------------------------------------------------
#
# Two modes:
#   • `make evals`         — stub LLM. Deterministic. Runs on every PR in CI.
#                            Verifies plumbing + the stats engine numbers,
#                            and gates against evals/council/baselines/stub.json.
#   • `make evals-real`    — real Kimi. Manual. Requires KIMI_API_KEY.
#                            Produces a quality measurement; can update the
#                            real-LLM baseline.
#
# The output JSON is the artefact a downstream service can attach to a
# PR comment via `aatxe comment --report …`.

EVALS_JSON     := $(TMP)/aatxe-evals.json
EVALS_MD       := $(TMP)/aatxe-evals.md
EVALS_BASELINE := $(REPO_ROOT)/evals/council/baselines/stub.json

.PHONY: evals
evals: $(TMP) build-rust ## Run the eval harness with the stub LLM and gate against the committed baseline
	$(call say,evals)
	AATXE_COUNCIL_STUB=1 $(AATXE_BIN) evals \
	    --out $(EVALS_JSON) \
	    --markdown $(EVALS_MD) \
	    --baseline $(EVALS_BASELINE)
	@echo "    ✓ evals: wrote $(EVALS_JSON) and $(EVALS_MD)"

.PHONY: evals-no-gate
evals-no-gate: $(TMP) build-rust ## Run the eval harness (stub) without baseline gating — for iterating on the corpus
	$(call say,evals-no-gate)
	AATXE_COUNCIL_STUB=1 $(AATXE_BIN) evals \
	    --out $(EVALS_JSON) \
	    --markdown $(EVALS_MD)
	@echo "    ✓ evals-no-gate: wrote $(EVALS_JSON) and $(EVALS_MD)"

.PHONY: evals-real
evals-real: $(TMP) build-rust ## Run the eval harness using the real Kimi backend. Requires KIMI_API_KEY.
	$(call say,evals-real)
	@if [ -z "$$KIMI_API_KEY" ]; then \
	    echo "    ✗ KIMI_API_KEY is unset. Export it (or source from mairu .env) and rerun."; \
	    exit 1; \
	fi
	$(AATXE_BIN) evals \
	    --council-real-llm \
	    --out $(TMP)/aatxe-evals-real.json \
	    --markdown $(TMP)/aatxe-evals-real.md
	@echo "    ✓ evals-real: wrote $(TMP)/aatxe-evals-real.{json,md}"

.PHONY: evals-real-gemini
evals-real-gemini: $(TMP) build-rust ## Run the eval harness using the real Gemini backend (direct HTTP, no gate). Requires GEMINI_API_KEY.
	$(call say,evals-real-gemini)
	@if [ -z "$$GEMINI_API_KEY" ]; then \
	    echo "    ✗ GEMINI_API_KEY is unset. Export it and rerun (model via GEMINI_MODEL, default gemini-2.5-flash)."; \
	    exit 1; \
	fi
	$(AATXE_BIN) evals \
	    --council-real-llm \
	    --backend gemini \
	    --out $(TMP)/aatxe-evals-real-gemini.json \
	    --markdown $(TMP)/aatxe-evals-real-gemini.md
	@echo "    ✓ evals-real-gemini: wrote $(TMP)/aatxe-evals-real-gemini.{json,md}"

.PHONY: evals-update-baseline
evals-update-baseline: evals-no-gate ## Replace the committed stub baseline with the current run. Use only when corpus or pipeline changes are deliberate.
	$(call say,evals-update-baseline)
	cp $(EVALS_JSON) $(EVALS_BASELINE)
	@echo "    ✓ wrote $(EVALS_BASELINE) — commit it to update the gate"

# ----------------------------------------------------------------------------
# M2.5 — real-claude baseline gate
# ----------------------------------------------------------------------------
EVALS_REAL_CLAUDE_BASELINE := $(REPO_ROOT)/evals/council/baselines/real-claude.json

.PHONY: evals-real-claude-gate
evals-real-claude-gate: $(TMP) build-rust ## Run real-LLM eval (claude-code backend) + gate against the committed real-claude baseline. Looser tolerances auto-applied. Requires Claude Code subscription on the box.
	$(call say,evals-real-claude-gate)
	@if ! command -v claude >/dev/null 2>&1; then \
	    echo "    ✗ \`claude\` binary not found on PATH — install Claude Code first."; \
	    exit 1; \
	fi
	$(AATXE_BIN) evals \
	    --council-real-llm \
	    --backend claude-code \
	    --out $(TMP)/aatxe-evals-real-claude.json \
	    --markdown $(TMP)/aatxe-evals-real-claude.md \
	    --baseline $(EVALS_REAL_CLAUDE_BASELINE)
	@echo "    ✓ evals-real-claude-gate: wrote $(TMP)/aatxe-evals-real-claude.{json,md}"

.PHONY: evals-update-real-claude-baseline
evals-update-real-claude-baseline: $(TMP) build-rust ## Replace the committed real-claude baseline with a fresh real-LLM run. Use deliberately on intentional improvements.
	$(call say,evals-update-real-claude-baseline)
	@if ! command -v claude >/dev/null 2>&1; then \
	    echo "    ✗ \`claude\` binary not found on PATH — install Claude Code first."; \
	    exit 1; \
	fi
	$(AATXE_BIN) evals \
	    --council-real-llm \
	    --backend claude-code \
	    --out $(EVALS_REAL_CLAUDE_BASELINE) \
	    --markdown $(REPO_ROOT)/evals/council/baselines/real-claude.md \
	    --no-fail
	@echo "    ✓ wrote $(EVALS_REAL_CLAUDE_BASELINE) — commit it to update the gate"

# ----------------------------------------------------------------------------
# M2.4 — offline confidence-floor recalibration
# ----------------------------------------------------------------------------
.PHONY: evals-recalibrate-floor
evals-recalibrate-floor: $(TMP) build-rust ## Sweep candidate confidence-floor values offline against a captured eval JSON. Set FROM=<path> (default tmp/aatxe-evals.json) and FLOORS=<csv> (default 0.50,0.55,0.60,0.65,0.70).
	$(call say,evals-recalibrate-floor)
	@FROM=$${FROM:-$(EVALS_JSON)}; \
	FLOORS=$${FLOORS:-0.50,0.55,0.60,0.65,0.70}; \
	if [ ! -f "$$FROM" ]; then \
	    echo "    ✗ FROM=$$FROM does not exist — run \`make evals\` or \`make evals-real-claude-gate\` first."; \
	    exit 1; \
	fi; \
	$(AATXE_BIN) evals \
	    --stats=false --council=false \
	    --recalibrate-from "$$FROM" \
	    --recalibrate-floors "$$FLOORS" \
	    --out $(TMP)/recalibration-sweep.md
	@echo "    ✓ evals-recalibrate-floor: also wrote $(TMP)/recalibration-sweep.md"

# ----------------------------------------------------------------------------
# Local CI via `act`
# ----------------------------------------------------------------------------

# act needs an arch image on Apple Silicon — the medium catthehacker image
# is small enough (~600MB) for our workloads. Override with ACT_IMAGE=...
ACT_IMAGE   ?= catthehacker/ubuntu:act-latest
ACT_FLAGS   := --container-architecture linux/amd64 \
               -P ubuntu-latest=$(ACT_IMAGE) \
               --pull=false

.PHONY: act-ci
act-ci: ## Run the `ci` workflow locally with act (needs Docker running)
	$(call say,act-ci)
	$(ACT) push --workflows .github/workflows/ci.yml $(ACT_FLAGS)

.PHONY: act-ci-rust
act-ci-rust: ## Run only the `rust` job of ci.yml locally
	$(call say,act-ci-rust)
	$(ACT) push -j rust --workflows .github/workflows/ci.yml $(ACT_FLAGS)

.PHONY: act-list
act-list: ## List workflows + jobs visible to act
	$(ACT) -l

.PHONY: act-self-bench
act-self-bench: ## Run the self-bench workflow under act (push event → self-compare, no base; proves the bench→compare→gate pipeline)
	$(call say,act-self-bench)
	$(ACT) push --workflows .github/workflows/aatxe-self-bench.yml $(ACT_FLAGS)

.PHONY: act-council
act-council: ## Run the council selftest workflow under act (stub-LLM by default; set KIMI_API_KEY + USE_REAL_KIMI=true for real Kimi)
	$(call say,act-council)
	@if [ "$$USE_REAL_KIMI" = "true" ] && [ -n "$$KIMI_API_KEY" ]; then \
	    echo "    → using real Kimi (USE_REAL_KIMI=true, KIMI_API_KEY set)"; \
	    $(ACT) workflow_dispatch \
	        --workflows .github/workflows/aatxe-council-selftest.yml \
	        --input use-real-kimi=true \
	        -s KIMI_API_KEY=$$KIMI_API_KEY \
	        $(ACT_FLAGS); \
	else \
	    echo "    → using stub LLM (no Moonshot calls). Set USE_REAL_KIMI=true + KIMI_API_KEY=... to flip."; \
	    $(ACT) workflow_dispatch \
	        --workflows .github/workflows/aatxe-council-selftest.yml \
	        $(ACT_FLAGS); \
	fi

# ----------------------------------------------------------------------------
# Housekeeping
# ----------------------------------------------------------------------------

.PHONY: install
install: build-rust ## Install the aatxe binary to ~/.cargo/bin
	$(call say,install)
	$(CARGO) install --path crates/aatxe --locked

.PHONY: clean
clean: ## Remove cargo target/, sdk/ts/dist, sdk/ts/node_modules, examples node_modules, tmp/
	$(call say,clean)
	$(CARGO) clean
	rm -rf sdk/ts/dist sdk/ts/node_modules
	rm -rf examples/ts-example/node_modules
	rm -rf $(TMP)
