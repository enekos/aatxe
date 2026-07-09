#!/usr/bin/env bash
# Aatxe pre-commit hook.
#
# Installed via `make install-hooks`. Runs only on files staged for commit.
# Fails fast on formatting, clippy, or TS type errors so `origin/master` stays
# green without a separate CI round-trip.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

staged=$(git diff --cached --name-only --diff-filter=ACM)
rust_staged=$(echo "$staged" | grep '\.rs$' || true)
ts_staged=$(echo "$staged" | grep '^sdk/ts/.*\.ts$' || true)

if [ -n "$rust_staged" ]; then
    printf '\033[1;34m▶ cargo fmt --check\033[0m\n'
    cargo fmt --all -- --check

    printf '\033[1;34m▶ cargo clippy\033[0m\n'
    cargo clippy --workspace --all-targets -- -D warnings
fi

if [ -n "$ts_staged" ]; then
    printf '\033[1;34m▶ tsc --noEmit\033[0m\n'
    cd sdk/ts
    if [ ! -d node_modules ]; then
        npm install --silent
    fi
    npx tsc --noEmit -p tsconfig.json
fi

printf '\033[1;32m✅ pre-commit checks passed\033[0m\n'
