<!-- aatxe:evals -->
## aatxe evals — 2026-06-03T14:26:16.170687000Z (0.1.0)

Council LLM: **real Kimi**

### Council quality (24 cases)

| metric | value |
|---|---|
| Cases fully recalled | 12/24 |
| Cases over `maxFindings` cap | 3 |
| Cases with judge error | 0 |
| Critical recall | 0.750 |
| Critical precision | 1.000 |
| Critical F1 | 0.857 |
| Severity calibration MAE | 0.304 (0=perfect, 3=max) |
| Judge Brier score | 0.351 (0=perfect, 0.25=chance) |
| False positives per case | 2.375 |
| Forbidden-path findings | 0 |
| Avg latency | 26000 ms |
| Tokens (prompt/completion) | 552 / 52936 |

<details><summary>Per-case breakdown</summary>

| case | caught/total | unmatched | forbidden | over_cap | brier |
|---|---|---|---|---|---|
| `security-password-logged` | 1/1 | 1 | 0 | — | 0.305 |
| `security-ssrf-no-allowlist` | 1/1 | 4 | 0 | — | 0.415 |
| `correctness-null-deref` | 1/1 | 3 | 0 | — | 0.407 |
| `correctness-off-by-one` | 1/1 | 2 | 0 | — | 0.381 |
| `correctness-unwrap-in-handler` | 1/1 | 1 | 0 | — | 0.309 |
| `perf-n-plus-one` | 1/1 | 2 | 0 | — | 0.325 |
| `maintainability-todo-doc` | 0/0 | 0 | 0 | — | 0.090 |
| `clean-tiny-rename` | 0/0 | 0 | 0 | — | 0.000 |
| `clean-doc-typo` | 0/0 | 0 | 0 | — | 0.000 |
| `forbidden-generated-code` | 0/0 | 0 | 0 | — | 0.000 |
| `security-jwt-fallback-secret` | 2/4 | 9 | 0 | yes | 0.442 |
| `correctness-cache-race-stale-ttl` | 1/2 | 3 | 0 | — | 0.338 |
| `perf-django-export-n-plus-one` | 4/5 | 6 | 0 | yes | 0.321 |
| `security-authz-idor-export-route` | 1/3 | 3 | 0 | — | 0.398 |
| `maintainability-rust-reinvents-counters` | 2/3 | 5 | 0 | — | 0.342 |
| `security-jwt-verify-order` | 0/2 | 4 | 0 | — | 0.412 |
| `correctness-rust-async-cancel-unsafe` | 1/3 | 0 | 0 | — | 0.027 |
| `correctness-go-context-cleanup-goroutine` | 1/2 | 1 | 0 | — | 0.221 |
| `security-sql-injection-builder-bypass` | 1/2 | 2 | 0 | — | 0.530 |
| `correctness-rust-deadlock-lock-ordering` | 1/1 | 2 | 0 | — | 0.210 |
| `correctness-go-channel-leak-no-close` | 1/2 | 2 | 0 | — | 0.298 |
| `security-tenant-leak-query` | 0/1 | 5 | 0 | yes | 0.477 |
| `perf-rust-hot-path-allocation` | 1/2 | 1 | 0 | — | 0.191 |
| `correctness-ts-async-error-swallow` | 1/1 | 1 | 0 | — | 0.193 |
</details>

