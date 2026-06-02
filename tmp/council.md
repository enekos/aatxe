<!-- aatxe:report -->
## Performance · no significant changes

Service `aatxe-council` (Rust) · base `HEAD` → head `HEAD` · threshold ±5% · α=0.05

<details><summary>Neutral (9)</summary>

| Bench | Base (median) | Head (median) | Δ | p95 Δ | CV (b→h) | p | Verdict |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| `diff::chunk_for_review` | 1.22µs | 1.22µs | 0.0% | 0.0% | 23%→23% | 1.00e0 | ⚪ Neutral |
| `diff::filter_ignored` | 14.32µs | 14.32µs | 0.0% | 0.0% | 11%→11% | 1.00e0 | ⚪ Neutral |
| `diff::parse_unified_diff` | 6.71µs | 6.71µs | 0.0% | 0.0% | 21%→21% | 1.00e0 | ⚪ Neutral |
| `parse::parse_findings_json` | 1.62µs | 1.62µs | 0.0% | 0.0% | 2%→2% | 1.00e0 | ⚪ Neutral |
| `parse::parse_judge_verdicts` | 10.19µs | 10.19µs | 0.0% | 0.0% | 5%→5% | 1.00e0 | ⚪ Neutral |
| `pipeline::run_council_with_stub` | 59.35µs | 59.35µs | 0.0% | 0.0% | 22%→22% | 1.00e0 | ⚪ Neutral |
| `prompt::build_judge_request` | 14.81µs | 14.81µs | 0.0% | 0.0% | 0.6%→0.6% | 1.00e0 | ⚪ Neutral |
| `prompt::build_proposer_request` | 777ns | 777ns | 0.0% | 0.0% | 6%→6% | 1.00e0 | ⚪ Neutral |
| `synth::dedup_and_rank` | 10.83µs | 10.83µs | 0.0% | 0.0% | 6%→6% | 1.00e0 | ⚪ Neutral |

</details>

<details><summary>Methodology</summary>

Both refs run on the same CI machine with identical toolchain, back-to-back. Each bench: warmup samples discarded, then adaptive sampling (auto-batched for sub-µs ops) until target CV 2% or time budget. Effect size: relative median delta. Significance: Mann–Whitney U two-tailed p-value (non-parametric, no normality assumption). Verdict: regression when |Δmedian| ≥ 5% AND p < 0.05 AND not noise-gated. Noise gate: max(CV_base, CV_head) > 25% AND |Δmedian| < 2 × max(CV).

</details>