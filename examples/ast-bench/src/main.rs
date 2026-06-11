//! Microbenchmarks for `aatxe-ast` — the tree-sitter symbol-scope extractor.
//!
//! Of all the council's pre-LLM work, AST parsing is the heaviest per-call
//! CPU cost: each changed file is parsed with tree-sitter (~1 ms/KLOC) and
//! walked to build the `FileGraph` (symbols + intra-file call edges +
//! imports) that gets injected into proposer prompts. On a large PR this is
//! the part of the non-network path most likely to regress — and it had no
//! bench coverage.
//!
//! We bench the three language describers via the public `describe` entry
//! point (the same path the CLI's `build_scope_for_review` takes) plus the
//! `render_scope_block` renderer that turns parsed graphs into the prompt
//! section. Inputs are **frozen snapshots** of real aatxe source committed
//! under `fixtures/` — realistic shapes, but stable across commits so the
//! regression gate stays meaningful.
//!
//! Emits a single `RunReport` JSON on stdout, service-tagged `aatxe-ast`.

use aatxe_ast::{describe, render_scope_block, Language};
use aatxe_bench::{bench, black_box, Suite};

const RUST_SRC: &str = include_str!("../fixtures/sample.rs");
const TS_SRC: &str = include_str!("../fixtures/sample.ts");
const GO_SRC: &str = include_str!("../fixtures/sample.go");

fn main() {
    let mut suite = Suite::new("aatxe-ast");

    // --- 1-3. Per-language parse + graph extraction ----------------------
    // The hot path: tree-sitter parse → symbol/edge/import walk.
    bench(&mut suite, "describe::rust", || {
        let g = describe(Language::Rust, "fixtures/sample.rs", black_box(RUST_SRC));
        black_box(g);
    });
    bench(&mut suite, "describe::ts", || {
        let g = describe(Language::Ts, "fixtures/sample.ts", black_box(TS_SRC));
        black_box(g);
    });
    bench(&mut suite, "describe::go", || {
        let g = describe(Language::Go, "fixtures/sample.go", black_box(GO_SRC));
        black_box(g);
    });

    // --- 4. Renderer: parsed graphs → prompt scope block -----------------
    // A multi-file workspace (all three fixtures) rendered into the bounded
    // markdown block the council injects. Parse once outside the loop so the
    // bench isolates rendering + cross-file caller attribution.
    let workspace = vec![
        (
            "crates/sample.rs".to_string(),
            describe(Language::Rust, "crates/sample.rs", RUST_SRC),
        ),
        (
            "sdk/sample.ts".to_string(),
            describe(Language::Ts, "sdk/sample.ts", TS_SRC),
        ),
        (
            "sdk/sample.go".to_string(),
            describe(Language::Go, "sdk/sample.go", GO_SRC),
        ),
    ];
    let changed = [
        "crates/sample.rs".to_string(),
        "sdk/sample.ts".to_string(),
        "sdk/sample.go".to_string(),
    ];
    bench(&mut suite, "scope::render_scope_block", || {
        let block = render_scope_block(black_box(&workspace), black_box(&changed));
        black_box(block);
    });

    suite.emit_stdout();
}
