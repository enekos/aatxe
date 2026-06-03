#!/usr/bin/env bash
#
# Confidence-floor calibration sweep.
#
# Re-runs the eval corpus at three candidate `--confidence-floor` values
# (0.55 = current, 0.60, 0.65) and reports a side-by-side metric table so
# the choice of floor is data-justified, not a guess. Backed by
# `aatxe evals` so the metrics scheme is the same one CI gates against.
#
# Usage:
#   scripts/calibrate-confidence-floor.sh <tmp-dir> <aatxe-bin>
#
# Env knobs (all optional):
#   AATXE_FLOORS  — space-separated list of floors to sweep. Default
#                   "0.55 0.60 0.65".
#   USE_REAL_KIMI — when "true", runs against real Kimi. Default off so
#                   the sweep is fast/free. Real-LLM calibration takes
#                   ~60 minutes per floor and should be triggered
#                   explicitly.
#   AATXE_CORPUS  — override the council corpus directory. Defaults to
#                   `evals/council/cases`.
#
# Exit codes:
#   0 — sweep completed; results printed.
#   1 — usage error.
#   2 — at least one floor produced a regression past the committed
#       baseline's tolerance (eval gate fired).
#
# The committed baseline at `evals/council/baselines/stub.json` is the
# 0.55 reference. Anything below that (e.g. raising the floor and losing
# critical-recall by more than the tolerance) trips the gate.

set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <tmp-dir> <aatxe-bin>" >&2
    exit 1
fi

TMP="$1"
AATXE="$2"
FLOORS="${AATXE_FLOORS:-0.55 0.60 0.65}"
CORPUS="${AATXE_CORPUS:-evals/council/cases}"
USE_REAL="${USE_REAL_KIMI:-false}"

mkdir -p "$TMP/calibrate"

# Path to the python comparator that already knows how to render a
# side-by-side metric diff for two eval JSONs.
COMPARE="scripts/compare-real-vs-stub.py"
if [ ! -x "$COMPARE" ]; then
    # The committed scripts are checked in without exec bit on some
    # systems (depends on git config); make it executable on demand.
    chmod +x "$COMPARE" 2>/dev/null || true
fi

echo "▶ confidence-floor sweep: floors=${FLOORS}, real_kimi=${USE_REAL}"
echo "  corpus: ${CORPUS}"
echo ""

# Run the sweep. Each iteration writes its JSON to
# `$TMP/calibrate/floor-<f>.json`. We don't pass `--baseline` here so
# the sweep itself doesn't gate — gating happens after the sweep, once,
# against the committed baseline.
declare -a JSONS=()
for floor in $FLOORS; do
    OUT="$TMP/calibrate/floor-${floor}.json"
    MD="$TMP/calibrate/floor-${floor}.md"
    JSONS+=("$OUT")

    # `set -u` chokes on `${ARR[@]}` for an empty array — guard with
    # the `${ARR[@]+...}` indirection so we don't have to disable nounset
    # for the duration of the call.
    EXTRA_ARGS=()
    if [ "$USE_REAL" = "true" ]; then
        EXTRA_ARGS+=(--council-real-llm)
    fi

    echo "  ▶ floor=${floor} → ${OUT}"
    set +e
    "$AATXE" evals \
        --corpus "$CORPUS" \
        --confidence-floor "$floor" \
        --out "$OUT" \
        --markdown "$MD" \
        --no-fail \
        ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"} \
        > "$TMP/calibrate/floor-${floor}.log" 2>&1
    rc=$?
    set -e
    if [ $rc -ne 0 ]; then
        echo "    ✗ aatxe evals failed for floor=${floor} (rc=$rc). Log tail:" >&2
        tail -20 "$TMP/calibrate/floor-${floor}.log" >&2
        exit $rc
    fi
done

echo ""
echo "▶ side-by-side metrics"
echo ""

# Print a single combined table. We compare each non-default floor
# against the 0.55 baseline (the first in the FLOORS list by convention)
# so the deltas are interpretable as "moving the floor would cost/save".
BASELINE_JSON="${JSONS[0]}"
for json in "${JSONS[@]:1}"; do
    floor=$(echo "$json" | sed -E 's@.*floor-([0-9.]+)\.json$@\1@')
    echo "## floor=${floor} vs floor=$(echo "$BASELINE_JSON" | sed -E 's@.*floor-([0-9.]+)\.json$@\1@')"
    python3 "$COMPARE" "$BASELINE_JSON" "$json"
    echo ""
done

# Lastly: gate the *current default floor's* JSON against the committed
# baseline so the sweep also re-validates that the headline number hasn't
# regressed since the last commit. This is the same gate `make evals`
# uses; we just re-run it explicitly so the calibration target's exit
# code reflects regression status.
COMMITTED_BASELINE="evals/council/baselines/stub.json"
if [ -f "$COMMITTED_BASELINE" ]; then
    echo "▶ regression gate vs ${COMMITTED_BASELINE} (0.55 floor)"
    set +e
    "$AATXE" evals \
        --corpus "$CORPUS" \
        --confidence-floor 0.55 \
        --out "$TMP/calibrate/gate.json" \
        --baseline "$COMMITTED_BASELINE" \
        > "$TMP/calibrate/gate.log" 2>&1
    rc=$?
    set -e
    if [ $rc -eq 2 ]; then
        echo "  ✗ regression past tolerance vs committed baseline. See log:" >&2
        tail -30 "$TMP/calibrate/gate.log" >&2
        exit 2
    fi
fi

echo ""
echo "    ✓ evals-calibrate: wrote ${TMP}/calibrate/{floor-*.json,floor-*.md,*.log}"
echo "      Promote a new floor by editing CouncilArgs::confidence_floor's default"
echo "      and re-running \`make evals-update-baseline\` to lock in the new metric."
