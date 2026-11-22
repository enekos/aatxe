//! Microbenchmarks for `aatxe-ast` — the tree-sitter symbol and import
//! extractor behind the `--affected` resolver.
//!
//! Each file is parsed with tree-sitter (~1 ms/KLOC) and walked to build a
//! `FileGraph` (symbols + intra-file call edges + imports). We bench the
//! three language describers via the public `describe` entry point. Inputs
//! are **frozen snapshots** of real aatxe source committed under
//! `fixtures/` — realistic shapes, but stable across commits so the
//! regression gate stays meaningful.
//!
//! Emits a single `RunReport` JSON on stdout, service-tagged `aatxe-ast`.

use aatxe_ast::{describe, Language};
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

    suite.emit_stdout();
}
