<!-- aatxe:evals -->
## aatxe evals — 2026-11-22T07:08:58.569310000Z (0.1.1)

### Stats engine (10 scenarios, 10 passed)

Observed null FPR: **0.005** (target ≤ 0.05). Borderline 6% regression TPR: **0.780**.

| scenario | regression | improvement | neutral (noisy) | mean p | pass |
|---|---|---|---|---|---|
| `null` | 0.005 | 0.000 | 0.995 (0.000) | 0.523 | ✓ |
| `regression-clear-10pct` | 0.995 | 0.000 | 0.005 (0.000) | 0.000 | ✓ |
| `regression-borderline-6pct` | 0.780 | 0.000 | 0.220 (0.000) | 0.000 | ✓ |
| `improvement-clear-10pct` | 0.000 | 1.000 | 0.000 (0.000) | 0.000 | ✓ |
| `noise-swamps-small-signal` | 0.000 | 0.000 | 1.000 (1.000) | 0.453 | ✓ |
| `below-threshold-2pct` | 0.015 | 0.000 | 0.985 (0.000) | 0.193 | ✓ |
| `null-run-drift-1-round` | 0.210 | 0.195 | 0.595 (0.000) | 0.036 | ✓ |
| `null-run-drift-5-rounds` | 0.015 | 0.005 | 0.980 (0.000) | 0.567 | ✓ |
| `regression-10pct-run-drift-5-rounds` | 0.810 | 0.000 | 0.190 (0.000) | 0.035 | ✓ |
| `regression-6pct-quiet-5-rounds` | 0.890 | 0.000 | 0.110 (0.000) | 0.008 | ✓ |

