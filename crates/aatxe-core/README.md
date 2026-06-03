# aatxe-core

Pure, IO-free core types + statistics + comparison logic for [aatxe](https://github.com/enekos/aatxe) — a statistical microbenchmark + regression-gate harness for TypeScript, Go, and Rust.

This crate is the algorithmic heart of the system. It is consumed by:

- [`aatxe-bench`](https://crates.io/crates/aatxe-bench) — the Rust authoring SDK.
- the `aatxe` CLI — the comparator + sticky-comment poster.

You probably do not want to depend on `aatxe-core` directly unless you are building a custom adapter or a comparator-equivalent tool. Most users want `aatxe-bench`.

## What's inside

- `types` — `RunReport`, `BenchRun`, `CompareReport` (camelCase serde).
- `stats` — Welford mean, Mann-Whitney-U with continuity + tie correction, MAD merge-walk, Welch-t (diagnostic).
- `compare` — three-signal verdict: effect size × significance × noise gate.
- `report` — sticky-marker markdown rendering.
- `affected` — regex import-graph + git-diff intersection, generic over `Language`.

## License

MIT. See `LICENSE` at the workspace root.
