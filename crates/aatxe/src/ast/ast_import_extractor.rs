//! CLI-side bridge from `aatxe-core::affected::ImportExtractor` to
//! `aatxe-ast::describe`.
//!
//! Lives in the CLI binary (not in `aatxe-core`) so the pure-logic crate
//! stays free of the tree-sitter grammars. The CLI is already shipping
//! those grammars for the council's scope block; reusing them here costs
//! nothing extra.
//!
//! Behaviour matches `aatxe-core::affected::extract_specifiers` shape so
//! downstream `resolve_import` stays untouched: every returned string is
//! a candidate file-edge specifier (`./foo`, `./alt/d.rs`, …) that
//! `is_relative_spec` then accepts or rejects per-language. The
//! correctness wins over the regex pass are: no false positives inside
//! string/comment literals (tree-sitter parses real syntax), and
//! coverage of TS `export … from "…"` re-exports + dynamic `import()` /
//! `require()` that the regex pass already caught but more brittly.

use aatxe_core::affected::ImportExtractor;
use aatxe_core::types::Language;

pub struct AstImportExtractor;

impl ImportExtractor for AstImportExtractor {
    fn extract(&self, src: &str, lang: Language) -> Vec<String> {
        // `aatxe-ast::describe` is pure — no IO, no globals — and falls
        // back to a regex extractor when the corresponding language
        // feature is compiled out. So this call is always safe and
        // always cheap (~ms per file).
        //
        // The `file_path` argument is only used to pick TSX vs TS in the
        // TS describer; for affected-set resolution the spec shape is
        // identical, so a synthetic path is fine.
        let synthetic_path = match lang {
            Language::Ts => "src.ts",
            Language::Go => "src.go",
            Language::Rust => "src.rs",
        };
        aatxe_ast::describe(lang, synthetic_path, src).file_edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_extracts_static_and_dynamic_imports() {
        // Smoke: the bridge produces every TS file-edge shape the
        // affected resolver downstream knows how to walk.
        let edges = AstImportExtractor.extract(
            "import { a } from './a';\nimport './b';\nexport { z } from '../z';\nconst d = import('./d');\nconst m = require('./m');\n",
            Language::Ts,
        );
        for want in ["./a", "./b", "../z", "./d", "./m"] {
            assert!(
                edges.iter().any(|s| s == want),
                "missing {want} in {edges:?}"
            );
        }
    }

    #[test]
    fn rust_extracts_mod_decls_and_include() {
        let edges = AstImportExtractor.extract(
            "pub mod a;\nmod b;\n#[path = \"alt/c.rs\"]\nmod c;\nfn _x() { include!(\"./d.rs\"); }\n",
            Language::Rust,
        );
        for want in ["./a", "./b", "./alt/c.rs", "./d.rs"] {
            assert!(
                edges.iter().any(|s| s == want),
                "missing {want} in {edges:?}"
            );
        }
    }

    #[test]
    fn go_extracts_module_and_relative_paths() {
        // For Go we deliberately mirror the regex's behaviour: emit
        // every import path; the affected resolver's `is_relative_spec`
        // then filters out the non-relative ones.
        let edges = AstImportExtractor.extract(
            "package x\nimport \"fmt\"\nimport \"./local\"\n",
            Language::Go,
        );
        assert!(edges.iter().any(|s| s == "fmt"));
        assert!(edges.iter().any(|s| s == "./local"));
    }
}
