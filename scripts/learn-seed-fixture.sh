#!/usr/bin/env bash
# learn-seed-fixture.sh — exercise the learning corpus harvest+compact
# cycle end-to-end with no Kimi, no GitHub. Synthesises a PR's worth of
# comments + reactions + a council report, runs `aatxe learn harvest`
# against them, and writes the resulting corpus to the path the caller
# provides. The Makefile target `learn-seed` is the entry point.
#
# Usage: scripts/learn-seed-fixture.sh <aatxe-bin> <out-corpus-path>
#
# Exit codes:
#   0 — success
#   1 — fixture or CLI error

set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <aatxe-bin> <out-corpus-path>" >&2
    exit 1
fi

aatxe="$1"
out="$2"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

# Synthesised PR comments. Mirrors the GitHub REST `issue/comments` shape
# that `aatxe learn harvest --comments-file` consumes — see `PrComment` in
# `crates/aatxe-learn/src/harvest.rs`.
cat > "$tmpdir/comments.json" <<'JSON'
[
  {
    "id": 9000,
    "body": "<!-- aatxe:council -->\n## Council review · 1 critical\nSee findings below.",
    "user_login": "github-actions[bot]",
    "author_association": "NONE",
    "reactions": {"plus_one": 3, "minus_one": 0, "heart": 1, "hooray": 0, "rocket": 0, "confused": 0},
    "created_at": "2026-06-01T10:00:00Z"
  },
  {
    "id": 9001,
    "body": "Nice.\naatxe: remember always validate URL host before any outbound fetch",
    "user_login": "alice",
    "author_association": "MEMBER",
    "reactions": {"plus_one": 1, "minus_one": 0, "heart": 0, "hooray": 0, "rocket": 0, "confused": 0},
    "created_at": "2026-06-01T11:00:00Z"
  },
  {
    "id": 9002,
    "body": "aatxe: good catch on 0",
    "user_login": "bob",
    "author_association": "COLLABORATOR",
    "reactions": {"plus_one": 0, "minus_one": 0, "heart": 0, "hooray": 0, "rocket": 0, "confused": 0},
    "created_at": "2026-06-01T11:15:00Z"
  },
  {
    "id": 9003,
    "body": "actually disagree on the nit.\naatxe: false-positive on 1",
    "user_login": "carol",
    "author_association": "OWNER",
    "reactions": {"plus_one": 0, "minus_one": 0, "heart": 0, "hooray": 0, "rocket": 0, "confused": 0},
    "created_at": "2026-06-01T12:00:00Z"
  }
]
JSON

# Synthesised council report whose shipped findings line up with the
# `good catch on 0` / `false-positive on 1` directives above. The judge
# verdicts and confidences put both findings above the default
# confidence floor (0.55) so they both ship.
cat > "$tmpdir/council.json" <<'JSON'
{
  "model": "kimi-k2.6",
  "repo": "x/y",
  "pr": 7,
  "filesTotal": 2,
  "filesReviewed": 2,
  "proposerReviews": [],
  "synthesized": [],
  "judged": [
    {
      "finding": {
        "file": "src/fetch.rs",
        "line": 42,
        "severity": "critical",
        "category": "security",
        "title": "SSRF in fetch helper",
        "rationale": "url host is not validated"
      },
      "verdict": "keep",
      "confidence": 0.95
    },
    {
      "finding": {
        "file": "src/util/parse.rs",
        "line": 10,
        "severity": "minor",
        "category": "maintainability",
        "title": "naming nit",
        "rationale": "rename for clarity"
      },
      "verdict": "keep",
      "confidence": 0.70
    }
  ],
  "confidenceFloor": 0.55,
  "totalDurationMs": 1234,
  "totalPromptTokens": 0,
  "totalCompletionTokens": 0
}
JSON

# Start from an empty corpus so the run is reproducible.
rm -f "$out"

"$aatxe" learn harvest \
    --corpus "$out" \
    --pr 7 --repo x/y \
    --comments-file "$tmpdir/comments.json" \
    --council-report "$tmpdir/council.json"
