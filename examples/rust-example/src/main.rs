//! Minimal example of an aatxe-driven Rust bench runner.
//!
//! Invoked by `aatxe run --lang rust` via `AATXE_RUST_RUNNER` (defaults to
//! `cargo run --release -q --bin aatxe-rust-runner -- --json`).
//!
//! The runner emits a single RunReport JSON on stdout — everything else
//! goes to stderr so it doesn't pollute the payload.

use aatxe_bench::{bench, Suite};

fn main() {
    let mut suite = Suite::new("example-rust");

    // Cheap bench: relies on the auto-batched sampler.
    bench(&mut suite, "u64_add", || {
        let _ = std::hint::black_box(1u64.wrapping_add(2));
    });

    // Slightly heavier: vector allocation.
    bench(&mut suite, "vec_alloc", || {
        let v: Vec<u64> = (0..32).collect();
        std::hint::black_box(v);
    });

    suite.emit_stdout();
}
