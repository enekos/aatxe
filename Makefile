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
PNPM        ?= pnpm
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

.PHONY: ui-build
ui-build: ## Rebuild the Svelte dashboard bundle into crates/aatxe-ui/assets (commit the result). Only needed after editing crates/aatxe-ui/ui. Uses pnpm.
	$(call say,ui-build)
	cd crates/aatxe-ui/ui && [ -d node_modules ] || $(PNPM) install
	cd crates/aatxe-ui/ui && $(PNPM) run build

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
e2e: e2e-rust e2e-go e2e-ts e2e-regression-gate core-bench-self ast-bench-self evals ## Run the full aatxe pipeline against every adapter + a fake regression + self-bench compares (core/ast) + the stats eval harness

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
# Self-benches — aatxe benching its own hot code (very meta)
# ----------------------------------------------------------------------------
#
# `core-bench` covers the statistical brain (`aatxe-core`): summarize_samples,
# Mann-Whitney U, MAD, Welch-t, compare_reports, and the affected-set import
# extractor — the no-network code that runs on every gate. `ast-bench` covers
# `aatxe-ast` tree-sitter parsing — the import extraction behind `--affected`. Both feed the self-bench CI workflow (`aatxe-self-bench.yml`).

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
#   BENCH     — `core` (default, ~5s) / `ast` / `all`.
#   VERBOSE   — set to anything to stream cargo + bench output.
#
# microVM knobs (predictable numbers via a libkrun guest — see microvm-setup):
#   ISOLATION — `local` (default) or `microvm`.
#   VM_CPUS   — guest vCPUs (default 2).   VM_MEM — guest RAM MiB (default 2048).
#   VM_IMAGE  — override the guest OCI image (default is per-language).

REF       ?= origin/master
BENCH     ?= core
ISOLATION ?=
VM_CPUS   ?=
VM_MEM    ?=
VM_IMAGE  ?=

# Flags appended to `perf-vs` from the microVM knobs above (empty ⇒ host run).
vmflags = $(if $(ISOLATION),--isolation $(ISOLATION),) $(if $(VM_CPUS),--vm-cpus $(VM_CPUS),) $(if $(VM_MEM),--vm-mem $(VM_MEM),) $(if $(VM_IMAGE),--vm-image $(VM_IMAGE),)

.PHONY: perf-vs
perf-vs: $(TMP) build-rust ## Local A/B perf-compare HEAD vs $(REF) via a sibling worktree. Knobs: REF, BENCH, VERBOSE.
	$(call say,perf-vs)
	$(AATXE_BIN) perf-vs --against $(REF) --bench $(BENCH) \
	    --out-dir $(TMP)/perf-vs/$(BENCH) \
	    $(if $(VERBOSE),--verbose,) $(vmflags)
	@echo "    ✓ perf-vs: see $(TMP)/perf-vs/$(BENCH)/{head,base}.json + cmp.{json,md}"

.PHONY: perf-vs-self
perf-vs-self: $(TMP) build-rust ## Smoke test: perf-vs against HEAD~1 (or origin/master if no prior commit). Exercises the worktree + compare path end-to-end.
	$(call say,perf-vs-self)
	@ref=$$(git rev-parse --verify --quiet HEAD~1 || git rev-parse --verify origin/master); \
	   $(AATXE_BIN) perf-vs --against $$ref --bench core \
	       --out-dir $(TMP)/perf-vs/self
	@echo "    ✓ perf-vs-self: see $(TMP)/perf-vs/self/cmp.md"

# ----------------------------------------------------------------------------
# microVM benches (predictable numbers via a libkrun guest)
# ----------------------------------------------------------------------------
#
# `--isolation microvm` runs the bench inside an ephemeral libkrun microVM
# (pinned vCPUs, fixed RAM, clean rootfs) so numbers don't drift with host
# load. Works on macOS (Hypervisor.framework) and Linux (KVM). One-time
# `microvm-setup` installs `krunvm`; the first run pulls the guest image.

.PHONY: perf-vs-vm
perf-vs-vm: ## perf-vs inside a libkrun microVM. Knobs: REF, BENCH, VM_CPUS, VM_MEM, VM_IMAGE.
	@$(MAKE) --no-print-directory perf-vs ISOLATION=microvm

.PHONY: microvm-setup
microvm-setup: ## One-time: install krunvm (the libkrun microVM runner).
	$(call say,microvm-setup)
	@if [ "$$(uname)" = "Darwin" ]; then \
	    command -v brew >/dev/null 2>&1 || { echo "    ✗ Homebrew required — see https://brew.sh"; exit 1; }; \
	    brew tap slp/krun && brew install krunvm && echo "    ✓ krunvm installed — run: make microvm-doctor"; \
	 else \
	    echo "    Linux: install krunvm from your distro (Fedora: dnf install krunvm) or"; \
	    echo "    build libkrun + krunvm from https://github.com/containers/krunvm"; \
	 fi

.PHONY: microvm-doctor
microvm-doctor: ## Check the microVM backend (krunvm present, host arch, version).
	$(call say,microvm-doctor)
	@echo "    host  : $$(uname -m) $$(uname -s)"
	@if command -v krunvm >/dev/null 2>&1; then \
	    echo "    krunvm: $$(krunvm --version 2>/dev/null | head -1)"; \
	    echo "    ✓ ready — run: make perf-vs-vm   (or: aatxe perf-vs --isolation microvm …)"; \
	 else \
	    echo "    ✗ krunvm not found — run: make microvm-setup"; exit 1; \
	 fi

# ----------------------------------------------------------------------------
# Local realtime dashboard
# ----------------------------------------------------------------------------
#
# `aatxe ui` serves the embedded dashboard: spawn coding agents in
# isolated worktrees, re-bench on every working-tree change, stream
# trajectories + tournament standings to the browser.
#
# Knobs (all optional):
#   UI_PORT     — port to bind. Default 4866.
#   UI_BACKEND  — agent backend: gemini (default) / claude / stub.
#   GEMINI_ENV  — env file sourced before launch (provides GEMINI_API_KEY).
#                 Defaults to the first of: ./.env, the jobs-ai-prompt-lab
#                 checkout next to this repo's parent.
#   UI_FLAGS    — extra flags, e.g. `--no-open`.

UI_PORT    ?= 4866
UI_BACKEND ?= gemini
GEMINI_ENV ?= $(firstword $(wildcard .env $(HOME)/eneko_projects/jobs-ai-prompt-lab/.env))

.PHONY: ui
ui: build-rust ## Launch the dashboard on $(UI_PORT) with the $(UI_BACKEND) agent backend. Knobs: UI_PORT, UI_BACKEND, GEMINI_ENV, UI_FLAGS.
	$(call say,ui)
	@set -a; if [ -n "$(GEMINI_ENV)" ] && [ -f "$(GEMINI_ENV)" ]; then . "$(GEMINI_ENV)"; fi; set +a; \
	exec $(AATXE_BIN) ui --port $(UI_PORT) --agent-backend $(UI_BACKEND) $(UI_FLAGS)

.PHONY: ui-demo
ui-demo: build-rust ## Dashboard demo: offline stub agent, no LLM spend. Spawn an agent from the browser to watch the full loop.
	$(call say,ui-demo)
	$(AATXE_BIN) ui --port $(UI_PORT) --agent-backend stub

# ----------------------------------------------------------------------------
# Eval harness — `aatxe evals`
# ----------------------------------------------------------------------------
#
# Synthetic A/B benchmark pairs with known ground truth, scored against the
# comparator. Deterministic; runs on every PR in CI and gates against the
# committed evals/baseline.json.

EVALS_JSON     := $(TMP)/aatxe-evals.json
EVALS_MD       := $(TMP)/aatxe-evals.md
EVALS_BASELINE := $(REPO_ROOT)/evals/baseline.json

.PHONY: evals
evals: $(TMP) build-rust ## Run the stats eval harness and gate against the committed baseline
	$(call say,evals)
	$(AATXE_BIN) evals \
	    --out $(EVALS_JSON) \
	    --markdown $(EVALS_MD) \
	    --baseline $(EVALS_BASELINE)
	@echo "    ✓ evals: wrote $(EVALS_JSON) and $(EVALS_MD)"

.PHONY: evals-update-baseline
evals-update-baseline: $(TMP) build-rust ## Replace the committed baseline with the current run. Use only when comparator changes are deliberate.
	$(call say,evals-update-baseline)
	$(AATXE_BIN) evals --out $(EVALS_BASELINE) --markdown $(EVALS_MD)
	@echo "    ✓ wrote $(EVALS_BASELINE) — commit it to update the gate"

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
