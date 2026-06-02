<!-- aatxe:report -->
## Performance · no significant changes

Service `example-rust` (Rust) · base `8dece15` → head `8dece15` · threshold ±5% · α=0.05

> ⚠ 2 benches had CV > 25%; their results were noise-gated.

<details><summary>Neutral (2)</summary>

| Bench | Base (median) | Head (median) | Δ | p95 Δ | CV (b→h) | p | Verdict |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| `u64_add` | 0ns | 0ns | 0.0% | 0.0% | 70%→70% | 1.00e0 | 🟡 Noisy |
| `vec_alloc` | 11ns | 11ns | 0.0% | 0.0% | 161%→161% | 1.00e0 | 🟡 Noisy |

</details>

<details><summary>Methodology</summary>

Both refs run on the same CI machine with identical toolchain, back-to-back. Each bench: warmup samples discarded, then adaptive sampling (auto-batched for sub-µs ops) until target CV 2% or time budget. Effect size: relative median delta. Significance: Mann–Whitney U two-tailed p-value (non-parametric, no normality assumption). Verdict: regression when |Δmedian| ≥ 5% AND p < 0.05 AND not noise-gated. Noise gate: max(CV_base, CV_head) > 25% AND |Δmedian| < 2 × max(CV).

</details>