#!/bin/bash
set -euo pipefail

# Build the big-diff benchmark in release mode.
cargo build --release --bin aatxe-big-diff-bench 2>/dev/null

# Run it once, stash JSON, then extract everything from it.
JSON=$(./target/release/aatxe-big-diff-bench 2>/dev/null | sed -n '/^{/,$p')

# Extract per-benchmark medians.
echo "$JSON" | jq -r '
    .runs[] |
    "METRIC " + (.name | gsub("::"; "_")) + "_µs=" + (.median | tostring)
'

# Derived metric: throughput from the huge parse median.
# Diff size is hard-coded at ≈ 100.8 MB.
HUGE_PARSE=$(echo "$JSON" | jq '.runs[] | select(.name == "diff::parse_huge") | .median')
if [[ -n "$HUGE_PARSE" && "$HUGE_PARSE" != "null" && "$HUGE_PARSE" != "0" ]]; then
    THRUPUT=$(echo "scale=3; 100.796 / ($HUGE_PARSE / 1000000)" | bc 2>/dev/null || echo "0")
    echo "METRIC throughput_mbps=$THRUPUT"
fi
