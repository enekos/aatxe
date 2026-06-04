<!-- aatxe:evals -->
## aatxe evals — 2026-06-02T19:25:32.009723000Z (0.1.0)

Council LLM: **real Kimi**

### Council quality (15 cases)

| metric | value |
|---|---|
| Cases fully recalled | 9/15 |
| Cases over `maxFindings` cap | 0 |
| Cases with judge error | 0 |
| Critical recall | 0.286 |
| Critical precision | 1.000 |
| Critical F1 | 0.444 |
| Severity calibration MAE | 0.400 (0=perfect, 3=max) |
| Judge Brier score | 0.494 (0=perfect, 0.25=chance) |
| False positives per case | 2.267 |
| Forbidden-path findings | 0 |
| Avg latency | 249904 ms |
| Tokens (prompt/completion) | 0 / 0 |

<details><summary>Per-case breakdown</summary>

| case | caught/total | unmatched | forbidden | over_cap | brier |
|---|---|---|---|---|---|
| `security-password-logged` | 0/1 | 2 | 0 | — | 0.732 |
| `security-ssrf-no-allowlist` | 1/1 | 3 | 0 | — | 0.463 |
| `correctness-null-deref` | 1/1 | 0 | 0 | — | 0.003 |
| `correctness-off-by-one` | 1/1 | 0 | 0 | — | 0.000 |
| `correctness-unwrap-in-handler` | 1/1 | 0 | 0 | — | 0.006 |
| `perf-n-plus-one` | 1/1 | 2 | 0 | — | 0.347 |
| `maintainability-todo-doc` | 0/0 | 0 | 0 | — | 0.000 |
| `clean-tiny-rename` | 0/0 | 1 | 0 | — | 0.490 |
| `clean-doc-typo` | 0/0 | 0 | 0 | — | 0.000 |
| `forbidden-generated-code` | 0/0 | 0 | 0 | — | 0.000 |
| `security-jwt-fallback-secret` | 1/4 | 5 | 0 | — | 0.580 |
| `correctness-cache-race-stale-ttl` | 1/2 | 7 | 0 | — | 0.581 |
| `perf-django-export-n-plus-one` | 2/5 | 7 | 0 | — | 0.440 |
| `security-authz-idor-export-route` | 0/3 | 3 | 0 | — | 0.622 |
| `maintainability-rust-reinvents-counters` | 1/3 | 4 | 0 | — | 0.605 |
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

