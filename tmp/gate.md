<!-- aatxe:report -->
## Performance · 1 regression

Service `gate-svc` (Rust) · base `aaaaaaa` → head `bbbbbbb` · threshold ±5% · α=0.05

### Significant changes

| Bench | Base (median) | Head (median) | Δ | p95 Δ | CV (b→h) | p | Verdict |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| `hot-path` | 130ns | 168ns | +30.0% | +30.0% | 13%→13% | 2.75e-14 | 🔴 Regression |

<details><summary>Methodology</summary>

Both refs run on the same CI machine with identical toolchain, back-to-back. Each bench: warmup samples discarded, then adaptive sampling (auto-batched for sub-µs ops) until target CV 2% or time budget. Effect size: relative median delta. Significance: Mann–Whitney U two-tailed p-value (non-parametric, no normality assumption). Verdict: regression when |Δmedian| ≥ 5% AND p < 0.05 AND not noise-gated. Noise gate: max(CV_base, CV_head) > 25% AND |Δmedian| < 2 × max(CV).

</details>