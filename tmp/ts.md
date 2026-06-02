<!-- aatxe:report -->
## Performance · no significant changes

Service `example-ts` (TypeScript) · base `8dece15` → head `8dece15` · threshold ±5% · α=0.05

> ⚠ 1 bench had CV > 25%; their results were noise-gated.

<details><summary>Neutral (2)</summary>

| Bench | Base (median) | Head (median) | Δ | p95 Δ | CV (b→h) | p | Verdict |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| `parse: number` | 60ns | 60ns | 0.0% | 0.0% | 209%→209% | 1.00e0 | 🟡 Noisy |
| `parse: small json` | 192ns | 192ns | 0.0% | 0.0% | 18%→18% | 1.00e0 | ⚪ Neutral |

</details>

<details><summary>Methodology</summary>

Both refs run on the same CI machine with identical toolchain, back-to-back. Each bench: warmup samples discarded, then adaptive sampling (auto-batched for sub-µs ops) until target CV 2% or time budget. Effect size: relative median delta. Significance: Mann–Whitney U two-tailed p-value (non-parametric, no normality assumption). Verdict: regression when |Δmedian| ≥ 5% AND p < 0.05 AND not noise-gated. Noise gate: max(CV_base, CV_head) > 25% AND |Δmedian| < 2 × max(CV).

</details>