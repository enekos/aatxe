# aatxe-bench

Rust authoring API + JSON emitter for [aatxe](https://github.com/enekos/aatxe) — a statistical microbenchmark + regression-gate harness for TypeScript, Go, and Rust.

## Quick start

```toml
[dev-dependencies]
aatxe-bench = "0.1"
```

Write a runner binary:

```rust
use aatxe_bench::{bench, keep, Suite};

fn main() {
    let mut suite = Suite::new("my-service");

    bench(&mut suite, "parse_phone", || {
        keep(parse_phone("+34 612 345 678"));
    });

    suite.emit_stdout();
}
```

The runner emits a single `RunReport` JSON on stdout. Wire it into `aatxe run --lang rust` and aatxe compares head vs. base, posts a sticky PR comment, and gates CI when a regression is statistically real.

## Why a builder, not `#[bench]`?

The stable Rust toolchain does not ship `#[bench]`. Criterion is the standard alternative but introduces a heavy dependency tree and an HTML-report-centric flow. `aatxe-bench` instead exposes a small builder that integrates cleanly with `cargo run --release` — predictable, statically typed, and trivial to embed in CI.

Sampling matches the cross-language defaults:

- warmup iterations excluded from measurement;
- adaptive sampling that stops once the CV drops below `Options::target_cv`, the time budget expires, or `Options::max_iterations` is hit;
- automatic batch sizing for sub-µs operations.

## Defeating dead-code elimination

```rust
use aatxe_bench::{black_box, keep};

bench(&mut suite, "parse_foo", || {
    keep(parse_foo("hello"));  // identity at the value level, defeats DCE
});
```

`keep` is named for parity with the TS (`keep`) and Go (`Keep`) SDKs. `black_box` is re-exported from `std::hint` for criterion-refugee muscle memory.

## License

MIT.
