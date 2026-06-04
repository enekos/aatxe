//! Regex-based fallback extractor.
//!
//! Lifted from mairu's `BaseExtract` (`language_describer.go:56`). Good
//! enough to surface top-level function and class names plus a coarse
//! call graph when a tree-sitter describer isn't available — used both
//! as the explicit unsupported-language fallback and as the off-feature
//! default in [`crate::describer::describe`].
//!
//! Precision is intentionally weak; the council will *additionally* see
//! the diff and full file context, so a missed symbol just means the
//! prompt's "Symbol scope" section is shorter — not that the review is
//! wrong. False positives are more painful than misses, so the patterns
//! err on the side of skipping ambiguous matches (e.g. `if (` is never
//! treated as a method call).

use crate::types::{FileGraph, LogicEdge, LogicSymbol, SymbolKind};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashMap;

static RE_FUNC: Lazy<Regex> = Lazy::new(|| {
    // Matches `pub fn name(`, `function name(`, `func name(`, with
    // optional `export ` / `pub ` / `async ` / `static ` modifiers.
    Regex::new(r"(?m)(?:export\s+|pub\s+(?:\([^)]*\)\s+)?|async\s+|static\s+)*(?:function|fn|func)\s+([A-Za-z_]\w*)\s*[(<]")
        .expect("compile RE_FUNC")
});
static RE_CLASS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)(?:export\s+|pub\s+)*class\s+([A-Za-z_]\w*)").expect("compile RE_CLASS")
});
static RE_STRUCT_OR_ENUM: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)(?:pub\s+(?:\([^)]*\)\s+)?)?(?:struct|enum|interface|type)\s+([A-Za-z_]\w*)")
        .expect("compile RE_STRUCT_OR_ENUM")
});
static RE_CALL: Lazy<Regex> = Lazy::new(|| {
    // `name(` — naive but good enough. Reserved control-flow keywords are
    // filtered after matching to keep the regex itself fast.
    Regex::new(r"([A-Za-z_]\w*)\s*\(").expect("compile RE_CALL")
});

const CONTROL_FLOW_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "catch", "function", "return", "match", "fn", "func", "case",
    "else", "do", "try", "throw", "select",
];

/// Best-effort extractor that uses regexes on the raw source. Always
/// succeeds — empty input yields an empty graph.
pub fn regex_extract(file_path: &str, source: &str) -> FileGraph {
    let mut symbols: Vec<LogicSymbol> = Vec::new();

    for (i, line) in source.lines().enumerate() {
        if let Some(c) = RE_FUNC.captures(line) {
            let name = &c[1];
            symbols.push(LogicSymbol {
                id: format!("fn:{name}"),
                name: name.to_string(),
                kind: SymbolKind::Function,
                exported: line.trim_start().starts_with("pub")
                    || line.trim_start().starts_with("export"),
                line: i + 1,
                signature: line.trim().to_string(),
                control_flow: String::new(),
                doc: String::new(),
            });
        }
        if let Some(c) = RE_CLASS.captures(line) {
            let name = &c[1];
            symbols.push(LogicSymbol {
                id: format!("cls:{name}"),
                name: name.to_string(),
                kind: SymbolKind::Class,
                exported: line.trim_start().starts_with("pub")
                    || line.trim_start().starts_with("export"),
                line: i + 1,
                signature: line.trim().to_string(),
                control_flow: String::new(),
                doc: String::new(),
            });
        }
        if let Some(c) = RE_STRUCT_OR_ENUM.captures(line) {
            let name = &c[1];
            // Don't double-count things the class regex already grabbed.
            if !symbols.iter().any(|s| s.name == name && s.line == i + 1) {
                symbols.push(LogicSymbol {
                    id: format!("type:{name}"),
                    name: name.to_string(),
                    kind: SymbolKind::Type,
                    exported: line.trim_start().starts_with("pub")
                        || line.trim_start().starts_with("export"),
                    line: i + 1,
                    signature: line.trim().to_string(),
                    control_flow: String::new(),
                    doc: String::new(),
                });
            }
        }
    }

    let mut ids_by_name: HashMap<&str, &str> = HashMap::new();
    for s in &symbols {
        ids_by_name.entry(s.name.as_str()).or_insert(s.id.as_str());
    }

    let mut edges: Vec<LogicEdge> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = Default::default();
    for c in RE_CALL.captures_iter(source) {
        let callee = &c[1];
        if CONTROL_FLOW_KEYWORDS.contains(&callee) {
            continue;
        }
        if let Some(to_id) = ids_by_name.get(callee) {
            // We don't know *from* which symbol the call originates with
            // a flat regex pass — attribute the edge to the file
            // virtual-symbol `file:<path>` so the renderer can still show
            // "this file calls X" entries when scoping. Mairu records
            // line-resolved attribution; the regex fallback intentionally
            // does not.
            let from = format!("file:{file_path}");
            let key = (from.clone(), (*to_id).to_string());
            if seen.insert(key) {
                edges.push(LogicEdge {
                    from,
                    to: (*to_id).to_string(),
                    kind: "call".to_string(),
                });
            }
        }
    }

    let imports = extract_imports_naive(source);
    // The fallback can't tell which language it's reading, so the
    // file_edges shape is "every import that looks file-edge-shaped" —
    // i.e. starts with `./` or `../`. That matches what
    // `aatxe-core::affected::is_relative_spec` accepts for TS/Go/Rust
    // (Rust's `mod foo;` is handled by the tree-sitter describer; the
    // fallback only ever runs when the corresponding feature is off, in
    // which case affected.rs's own regex extractor takes over anyway).
    let file_edges: Vec<String> = imports
        .iter()
        .filter(|s| s.starts_with("./") || s.starts_with("../"))
        .cloned()
        .collect();

    FileGraph {
        file_summary: format!("{} symbols", symbols.len()),
        raw_content: String::new(),
        symbols,
        edges,
        imports,
        file_edges,
        symbol_descriptions: HashMap::new(),
    }
}

static RE_IMPORT_LINE: Lazy<Regex> = Lazy::new(|| {
    // Catches:
    //   `import x from 'y'` / `import 'y'`           — JS/TS
    //   `import "y"` / `import ( … )`                — Go (line-by-line)
    //   `use crate::x::y;` / `extern crate y;`       — Rust
    Regex::new(
        r#"(?m)^\s*(?:import\s+(?:[^"']*\s+from\s+)?['"]([^'"]+)['"]|import\s+\(\s*["']([^'"]+)["']|use\s+([A-Za-z_][A-Za-z_0-9:\*\{\}\s,]*)\s*;|extern\s+crate\s+([A-Za-z_]\w*))"#,
    )
    .expect("compile RE_IMPORT_LINE")
});

fn extract_imports_naive(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in RE_IMPORT_LINE.captures_iter(source) {
        for i in 1..=4 {
            if let Some(m) = c.get(i) {
                let raw = m.as_str().trim();
                if !raw.is_empty() && !out.iter().any(|s: &String| s == raw) {
                    out.push(raw.to_string());
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_top_level_fn() {
        let g = regex_extract(
            "x.rs",
            "pub fn render() {}\nfn private_helper() {}\nfn _ignored() { return; }\n",
        );
        let names: Vec<&str> = g.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"render"));
        assert!(names.contains(&"private_helper"));
        // exported flag follows `pub` / `export`.
        let render = g.symbols.iter().find(|s| s.name == "render").unwrap();
        assert!(render.exported);
        let priv_ = g
            .symbols
            .iter()
            .find(|s| s.name == "private_helper")
            .unwrap();
        assert!(!priv_.exported);
    }

    #[test]
    fn extracts_ts_class_and_export() {
        let g = regex_extract("x.ts", "export class FooBar {\n  do() {}\n}\n");
        assert!(g.symbols.iter().any(|s| s.name == "FooBar" && s.exported));
    }

    #[test]
    fn extracts_go_struct_as_type() {
        let g = regex_extract("x.go", "package main\ntype User struct { Name string }\n");
        let u = g.symbols.iter().find(|s| s.name == "User").unwrap();
        assert_eq!(u.kind, SymbolKind::Type);
    }

    #[test]
    fn extracts_call_edges_excluding_keywords() {
        let g = regex_extract(
            "x.rs",
            "fn caller() {}\nfn callee() {}\nfn run() { caller(); callee(); if (true) {} }\n",
        );
        let to_set: std::collections::HashSet<&str> =
            g.edges.iter().map(|e| e.to.as_str()).collect();
        assert!(to_set.contains("fn:caller"));
        assert!(to_set.contains("fn:callee"));
        // `if(` MUST NOT become a call edge.
        assert!(!to_set.iter().any(|t| t.ends_with(":if")));
    }

    #[test]
    fn imports_are_collected_across_languages() {
        let src = "import x from 'y/z'\nimport \"github.com/foo/bar\"\nuse crate::stats::welch;\nextern crate serde;\n";
        let g = regex_extract("x.ts", src);
        assert!(g.imports.iter().any(|s| s == "y/z"));
        assert!(g.imports.iter().any(|s| s == "github.com/foo/bar"));
        assert!(g.imports.iter().any(|s| s.starts_with("crate::stats")));
        assert!(g.imports.iter().any(|s| s == "serde"));
    }
}
