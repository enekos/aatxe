#!/usr/bin/env bash
# Synthesise two RunReports where head is 30% slower than base, then assert
# that `aatxe compare --fail-on-regression` exits 2. This pins the CI gate.
set -euo pipefail

AATXE_BIN=${1:?aatxe binary path}
TMP=${2:?tmp directory}

base=$TMP/gate-base.json
head=$TMP/gate-head.json

python3 - "$base" "$head" <<'PY'
import json, sys

base_path, head_path = sys.argv[1:3]
base_samples = [100 + i for i in range(60)]
head_samples = [x * 1.30 for x in base_samples]

def report(samples, ref):
    return {
        "schemaVersion": 2,
        "language": "rust",
        "service": "gate-svc",
        "ref": ref,
        "runner": "synthetic",
        "startedAt": "2026-06-01T00:00:00Z",
        "finishedAt": "2026-06-01T00:00:01Z",
        "runs": [{
            "name": "hot-path",
            "file": "synthetic.rs",
            "iterations": len(samples),
            "batchSize": 1,
            "elapsedNs": float(sum(samples)),
            "samples": samples,
            "mean": 0.0, "median": 0.0, "trimmedMean": 0.0,
            "stddev": 0.0, "cv": 0.0, "mad": 0.0, "iqr": 0.0,
            "min": 0.0, "max": 0.0, "p50": 0.0, "p95": 0.0, "p99": 0.0,
        }],
    }

with open(base_path, "w") as f:
    json.dump(report(base_samples, "aaaaaaaaaa"), f)
with open(head_path, "w") as f:
    json.dump(report(head_samples, "bbbbbbbbbb"), f)
PY

set +e
"$AATXE_BIN" compare --base "$base" --head "$head" \
    --out "$TMP/gate.cmp.json" --markdown "$TMP/gate.md" \
    --fail-on-regression >/dev/null
code=$?
set -e

if [[ $code -ne 2 ]]; then
    echo "    ✗ regression gate: expected exit 2, got $code"
    exit 1
fi
echo "    ✓ regression gate: exit 2 as expected"

# Sanity-check the report content.
if ! grep -q '🔴 Regression' "$TMP/gate.md"; then
    echo "    ✗ regression gate: 🔴 Regression badge missing in markdown"
    exit 1
fi
echo "    ✓ regression gate: markdown carries 🔴 Regression badge"
