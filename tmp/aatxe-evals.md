<!-- aatxe:evals -->
## aatxe evals — 2026-06-03T14:44:17.908481000Z (0.1.0)

Council LLM: _stub (deterministic)_

### Council quality (24 cases)

| metric | value |
|---|---|
| Cases fully recalled | 6/24 |
| Cases over `maxFindings` cap | 6 |
| Cases with judge error | 0 |
| Critical recall | 0.062 |
| Critical precision | 1.000 |
| Critical F1 | 0.118 |
| Severity calibration MAE | 0.000 (0=perfect, 3=max) |
| Judge Brier score | 0.526 (0=perfect, 0.25=chance) |
| False positives per case | 4.625 |
| Forbidden-path findings | 0 |
| Avg latency | 0 ms |
| Tokens (prompt/completion) | 0 / 0 |

<details><summary>Per-case breakdown</summary>

| case | caught/total | unmatched | forbidden | over_cap | brier |
|---|---|---|---|---|---|
| `security-password-logged` | 1/1 | 4 | 0 | — | 0.363 |
| `security-ssrf-no-allowlist` | 1/1 | 3 | 0 | — | 0.343 |
| `correctness-null-deref` | 0/1 | 5 | 0 | — | 0.543 |
| `correctness-off-by-one` | 0/1 | 5 | 0 | — | 0.543 |
| `correctness-unwrap-in-handler` | 0/1 | 5 | 0 | — | 0.543 |
| `perf-n-plus-one` | 0/1 | 5 | 0 | — | 0.543 |
| `maintainability-todo-doc` | 0/0 | 4 | 0 | — | 0.523 |
| `clean-tiny-rename` | 0/0 | 5 | 0 | yes | 0.543 |
| `clean-doc-typo` | 0/0 | 5 | 0 | yes | 0.543 |
| `forbidden-generated-code` | 0/0 | 0 | 0 | — | 0.000 |
| `security-jwt-fallback-secret` | 0/4 | 5 | 0 | — | 0.543 |
| `correctness-cache-race-stale-ttl` | 0/2 | 5 | 0 | — | 0.543 |
| `perf-django-export-n-plus-one` | 0/5 | 5 | 0 | — | 0.543 |
| `security-authz-idor-export-route` | 0/3 | 5 | 0 | — | 0.543 |
| `maintainability-rust-reinvents-counters` | 0/3 | 5 | 0 | — | 0.543 |
| `security-jwt-verify-order` | 0/2 | 5 | 0 | — | 0.543 |
| `correctness-rust-async-cancel-unsafe` | 0/3 | 5 | 0 | — | 0.543 |
| `correctness-go-context-cleanup-goroutine` | 0/2 | 5 | 0 | — | 0.543 |
| `security-sql-injection-builder-bypass` | 0/2 | 5 | 0 | — | 0.543 |
| `correctness-rust-deadlock-lock-ordering` | 0/1 | 5 | 0 | yes | 0.543 |
| `correctness-go-channel-leak-no-close` | 0/2 | 5 | 0 | — | 0.543 |
| `security-tenant-leak-query` | 0/1 | 5 | 0 | yes | 0.543 |
| `perf-rust-hot-path-allocation` | 0/2 | 5 | 0 | yes | 0.543 |
| `correctness-ts-async-error-swallow` | 0/1 | 5 | 0 | yes | 0.543 |
</details>

### Stats engine (6 scenarios, 6 passed)

Observed null FPR: **0.000** (target ≤ 0.05). Borderline 6% regression TPR: **0.805**.

| scenario | regression | improvement | neutral (noisy) | mean p | pass |
|---|---|---|---|---|---|
| `null` | 0.000 | 0.000 | 1.000 (0.000) | 0.495 | ✓ |
| `regression-clear-10pct` | 1.000 | 0.000 | 0.000 (0.000) | 0.000 | ✓ |
| `regression-borderline-6pct` | 0.805 | 0.000 | 0.195 (0.000) | 0.000 | ✓ |
| `improvement-clear-10pct` | 0.000 | 1.000 | 0.000 (0.000) | 0.000 | ✓ |
| `noise-swamps-small-signal` | 0.000 | 0.000 | 1.000 (0.990) | 0.456 | ✓ |
| `below-threshold-2pct` | 0.005 | 0.000 | 0.995 (0.000) | 0.182 | ✓ |

