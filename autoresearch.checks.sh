#!/bin/bash
set -euo pipefail
# Fast correctness checks — must pass before a result can be kept.
cargo test --workspace --quiet 2>&1 | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
