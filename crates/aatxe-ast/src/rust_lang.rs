//! Tree-sitter Rust describer.
//!
//! Mairu doesn't have a Rust describer; this one is net-new. It targets
//! the symbols a code reviewer most often wants to scope: `fn`, `impl`
//! methods, `struct` / `enum` / `type` declarations, plus an intra-file
//! call graph that resolves `name(` callsites against the file's own
//! symbol table.

use crate::describer::LanguageDescriber;
use crate::types::{FileGraph, LogicEdge, LogicSymbol, SymbolKind};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Parser};

pub struct RustDescriber;

impl LanguageDescriber for RustDescriber {
    fn language_id(&self) -> &'static str {
        "rust"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }
    fn extract_file_graph(&self, file_path: &str, source: &str) -> FileGraph {
        let mut parser = Parser::new();
        if parser.set_language(&tree_sitter_rust::language()).is_err() {
            return crate::base::regex_extract(file_path, source);
        }
        let Some(tree) = parser.parse(source, None) else {
            return crate::base::regex_extract(file_path, source);
        };
        let bytes = source.as_bytes();
        let root = tree.root_node();

        let mut symbols: Vec<LogicSymbol> = Vec::new();
        let mut imports: Vec<String> = Vec::new();
        let mut file_edges: Vec<String> = Vec::new();

        walk(
            root,
            bytes,
            source,
            &mut symbols,
            &mut imports,
            &mut file_edges,
            None,
        );

        let edges = call_edges(&symbols, source);

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
}

fn walk(
    node: Node,
    bytes: &[u8],
    source: &str,
    out: &mut Vec<LogicSymbol>,
    imports: &mut Vec<String>,
    file_edges: &mut Vec<String>,
    impl_owner: Option<&str>,
) {
    let kind = node.kind();
    match kind {
        "function_item" => {
            if let Some(name) = field_text(node, "name", bytes) {
                let exported = has_pub_modifier(node, bytes);
                let signature = signature_line(node, source);
                let cf = control_flow_summary(node, bytes);
                let doc = doc_comment_before(node, source, bytes);
                let (id, sym_kind) = if let Some(owner) = impl_owner {
                    (format!("mtd:{owner}.{name}"), SymbolKind::Method)
                } else {
                    (format!("fn:{name}"), SymbolKind::Function)
                };
                out.push(LogicSymbol {
                    id,
                    name: name.to_string(),
                    kind: sym_kind,
                    exported,
                    line: node.start_position().row + 1,
                    signature,
                    control_flow: cf,
                    doc,
                });
            }
        }
        "struct_item" | "enum_item" | "type_item" | "union_item" | "trait_item" => {
            if let Some(name) = field_text(node, "name", bytes) {
                let exported = has_pub_modifier(node, bytes);
                let signature = signature_line(node, source);
                let doc = doc_comment_before(node, source, bytes);
                let kind = if node.kind() == "trait_item" {
                    SymbolKind::Class
                } else {
                    SymbolKind::Type
                };
                out.push(LogicSymbol {
                    id: format!("{}:{}", kind.tag(), name),
                    name: name.to_string(),
                    kind,
                    exported,
                    line: node.start_position().row + 1,
                    signature,
                    control_flow: String::new(),
                    doc,
                });
            }
        }
        "impl_item" => {
            // Recurse into the impl's body with the type-name as owner so
            // inner `function_item`s become methods.
            let owner = field_text(node, "type", bytes)
                .map(|s| s.to_string())
                .or_else(|| field_text(node, "trait", bytes).map(|s| s.to_string()))
                .unwrap_or_else(|| "impl".to_string());
            for child in named_children(node) {
                walk(child, bytes, source, out, imports, file_edges, Some(&owner));
            }
            return;
        }
        "use_declaration" => {
            // Capture the entire `use foo::bar::Baz;` body as one import
            // entry. Verbatim — splitting `use {a, b};` into individual
            // imports is more code than the council needs.
            if let Some(arg) = node.child_by_field_name("argument") {
                let text = arg.utf8_text(bytes).unwrap_or("").trim();
                if !text.is_empty() && !imports.iter().any(|s| s == text) {
                    imports.push(text.to_string());
                }
            } else {
                let text = node.utf8_text(bytes).unwrap_or("");
                let trimmed = text.trim_start_matches("use ").trim_end_matches(';').trim();
                if !trimmed.is_empty() && !imports.iter().any(|s| s == trimmed) {
                    imports.push(trimmed.to_string());
                }
            }
        }
        "extern_crate_declaration" => {
            if let Some(name) = field_text(node, "name", bytes) {
                if !imports.iter().any(|s| s == name) {
                    imports.push(name.to_string());
                }
            }
        }
        "mod_item" => {
            if let Some(body) = node.child_by_field_name("body") {
                // Inline module — recurse with the same owner context.
                // Don't emit a symbol for the module itself; the reviewer
                // cares about the functions inside.
                for child in named_children(body) {
                    walk(child, bytes, source, out, imports, file_edges, impl_owner);
                }
            } else if let Some(name) = field_text(node, "name", bytes) {
                // Non-inline `mod foo;` — emit a file edge. If a sibling
                // `#[path = "…"]` attribute precedes the mod_item, the
                // attribute overrides where `foo` lives on disk; capture
                // that synthetic specifier in lieu of `./{name}`.
                let edge = path_attr_before(node, bytes)
                    .map(|p| prefix_relative(&p))
                    .unwrap_or_else(|| format!("./{name}"));
                if !file_edges.iter().any(|s| s == &edge) {
                    file_edges.push(edge);
                }
            }
            return;
        }
        "macro_invocation" => {
            // `include!("path.rs")` — file-local. Resolve relative to the
            // current source file via the synthetic `./` prefix.
            //
            // The tree-sitter-rust grammar's "macro" field can return
            // either `include` or `include!` depending on grammar
            // version; check both.
            let macro_name = field_text(node, "macro", bytes).unwrap_or("");
            if macro_name == "include" || macro_name == "include!" {
                if let Some(arg) = include_arg(node, bytes) {
                    let edge = prefix_relative(&arg);
                    if !file_edges.iter().any(|s| s == &edge) {
                        file_edges.push(edge);
                    }
                }
            }
        }
        _ => {}
    }
    for child in named_children(node) {
        walk(child, bytes, source, out, imports, file_edges, impl_owner);
    }
}

/// `#[path = "alt/d.rs"]` immediately above a `mod_item` — returns the
/// quoted path, or None if no such attribute is attached.
fn path_attr_before<'a>(mod_node: Node<'a>, bytes: &'a [u8]) -> Option<String> {
    let parent = mod_node.parent()?;
    let mut prev: Option<Node<'a>> = None;
    let mut cursor = parent.walk();
    for sibling in parent.named_children(&mut cursor) {
        if sibling.id() == mod_node.id() {
            break;
        }
        prev = Some(sibling);
    }
    let attr = prev?;
    if attr.kind() != "attribute_item" {
        return None;
    }
    // `#[path = "alt/d.rs"]` is `attribute_item(attribute(identifier "path" =
    // string_literal))`. Pull the string literal out and unquote it.
    let text = attr.utf8_text(bytes).ok()?;
    let after_eq = text.split_once('=')?.1;
    let trimmed = after_eq.trim().trim_end_matches(']').trim();
    let unquoted = trimmed.trim_start_matches('"').trim_end_matches('"');
    if text.contains("path") && !unquoted.is_empty() {
        Some(unquoted.to_string())
    } else {
        None
    }
}

/// Extract the first string-literal argument of an `include!(...)`
/// macro_invocation, with the surrounding quotes stripped. tree-sitter-
/// rust's `macro_invocation` carries the bracketed body as a `token_tree`
/// sibling rather than via a named field — walk the whole subtree
/// looking for the first `string_literal` and pull its inner text.
fn include_arg<'a>(node: Node<'a>, bytes: &'a [u8]) -> Option<String> {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "string_literal" {
            // tree-sitter-rust wraps the string body in a `string_content`
            // inner node; fall back to the raw text and trim quotes if
            // the inner node isn't present.
            if let Some(inner) = child.named_child(0) {
                if let Ok(text) = inner.utf8_text(bytes) {
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
            if let Ok(text) = child.utf8_text(bytes) {
                return Some(text.trim_matches('"').to_string());
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() || cursor.node().id() == node.id() {
                return None;
            }
        }
    }
}

/// Prefix a bare path with `./` so it round-trips through
/// `aatxe-core::affected::is_relative_spec` and `resolve_import`.
fn prefix_relative(p: &str) -> String {
    if p.starts_with("./") || p.starts_with("../") || p.starts_with('/') {
        p.to_string()
    } else {
        format!("./{p}")
    }
}

fn field_text<'a>(node: Node<'a>, field: &str, bytes: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(bytes).ok())
}

fn has_pub_modifier(node: Node, bytes: &[u8]) -> bool {
    for child in named_children(node) {
        if child.kind() == "visibility_modifier" {
            let txt = child.utf8_text(bytes).unwrap_or("");
            return txt.starts_with("pub");
        }
    }
    false
}

fn signature_line(node: Node, source: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    let slice = &source.as_bytes()[start..end.min(source.len())];
    let s = std::str::from_utf8(slice).unwrap_or("");
    // Trim at the first `{` (body opener) so we keep just the signature.
    let cut = s.find('{').unwrap_or(s.len());
    s[..cut].split_whitespace().collect::<Vec<_>>().join(" ")
}

fn control_flow_summary(node: Node, _bytes: &[u8]) -> String {
    let (loops, conditionals) = count_loops_conds(node);
    if loops == 0 && conditionals == 0 {
        return String::new();
    }
    format!("loops={loops} conditionals={conditionals}")
}

fn count_loops_conds(node: Node) -> (usize, usize) {
    let mut loops = 0usize;
    let mut conds = 0usize;
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        match c.kind() {
            "for_expression" | "while_expression" | "loop_expression" => loops += 1,
            "if_expression" | "match_expression" => conds += 1,
            _ => {}
        }
        let (sl, sc) = count_loops_conds(c);
        loops += sl;
        conds += sc;
    }
    (loops, conds)
}

fn doc_comment_before(node: Node, source: &str, bytes: &[u8]) -> String {
    let mut sib = node.prev_sibling();
    let mut collected: Vec<String> = Vec::new();
    while let Some(s) = sib {
        if s.kind() == "line_comment" || s.kind() == "block_comment" {
            let txt = s.utf8_text(bytes).unwrap_or("");
            if txt.starts_with("///") || txt.starts_with("//!") || txt.starts_with("/**") {
                collected.push(strip_comment_markers(txt));
                sib = s.prev_sibling();
                continue;
            }
        }
        break;
    }
    collected.reverse();
    let joined = collected.join(" ");
    let _ = source;
    if joined.len() > 240 {
        let mut t = joined;
        t.truncate(237);
        t.push('…');
        t
    } else {
        joined
    }
}

fn strip_comment_markers(s: &str) -> String {
    s.lines()
        .map(|l| {
            l.trim_start()
                .trim_start_matches("///")
                .trim_start_matches("//!")
                .trim_start_matches("/**")
                .trim_end_matches("*/")
                .trim_start_matches('*')
                .trim()
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn named_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Naive but useful: scan the source for `name(` callsites whose `name`
/// is one of the file's own declared symbols, and attribute the edge
/// to the enclosing symbol. We don't try to be precise — false positives
/// here are harmless (the council prompt is already noisy enough that an
/// extra edge won't sway the model), and false negatives just shrink the
/// "called by" lists in `scope.rs`.
fn call_edges(symbols: &[LogicSymbol], source: &str) -> Vec<LogicEdge> {
    let mut ids_by_name: HashMap<&str, &str> = HashMap::new();
    for s in symbols {
        ids_by_name.entry(s.name.as_str()).or_insert(s.id.as_str());
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut edges: Vec<LogicEdge> = Vec::new();
    let call_re = regex::Regex::new(r"([A-Za-z_]\w*)\s*\(").expect("compile call_re");

    // Build a line→symbol-id map by sorting symbols by line and
    // bucketing every call's line under the most-recent symbol declared
    // before it.
    let mut sorted: Vec<&LogicSymbol> = symbols.iter().collect();
    sorted.sort_by_key(|s| s.line);

    for cap in call_re.captures_iter(source) {
        let m = cap.get(0).unwrap();
        let name = &cap[1];
        if matches!(
            name,
            "if" | "for"
                | "while"
                | "match"
                | "fn"
                | "loop"
                | "return"
                | "let"
                | "const"
                | "use"
                | "mod"
                | "as"
                | "in"
        ) {
            continue;
        }
        let Some(to_id) = ids_by_name.get(name) else {
            continue;
        };
        let line = source[..m.start()].matches('\n').count() + 1;
        let from = enclosing_symbol_id(&sorted, line).unwrap_or("file");
        if from == *to_id {
            continue;
        }
        let key = (from.to_string(), (*to_id).to_string());
        if seen.insert(key) {
            edges.push(LogicEdge {
                from: from.to_string(),
                to: (*to_id).to_string(),
                kind: "call".to_string(),
            });
        }
    }
    edges
}

fn enclosing_symbol_id<'a>(sorted: &'a [&'a LogicSymbol], line: usize) -> Option<&'a str> {
    let mut best: Option<&'a LogicSymbol> = None;
    for s in sorted {
        if s.line <= line {
            best = Some(*s);
        } else {
            break;
        }
    }
    best.map(|s| s.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn describe(src: &str) -> FileGraph {
        RustDescriber.extract_file_graph("x.rs", src)
    }

    #[test]
    fn extracts_top_level_fn() {
        let g = describe("pub fn render() -> String { String::new() }\n");
        let s = g.symbols.iter().find(|s| s.name == "render").unwrap();
        assert!(s.exported);
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(s.signature.contains("pub fn render"));
        assert_eq!(s.line, 1);
    }

    #[test]
    fn impl_methods_become_methods_with_owner_prefix() {
        let g = describe(
            "pub struct DiffParser;\nimpl DiffParser {\n    pub fn new() -> Self { Self }\n    fn helper(&self) {}\n}\n",
        );
        // Struct symbol present.
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "DiffParser" && s.kind == SymbolKind::Type));
        // Both methods present with `DiffParser.` prefix.
        let new = g.symbols.iter().find(|s| s.name == "new").unwrap();
        assert_eq!(new.kind, SymbolKind::Method);
        assert_eq!(new.id, "mtd:DiffParser.new");
        assert!(new.exported, "pub fn new => exported");
        let helper = g.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert!(!helper.exported);
    }

    #[test]
    fn struct_enum_type_get_emitted() {
        let g = describe("pub struct A;\nenum B { X, Y }\ntype C = A;\n");
        let names: Vec<&str> = g.symbols.iter().map(|s| s.name.as_str()).collect();
        for n in ["A", "B", "C"] {
            assert!(names.contains(&n), "missing {n} in {names:?}");
        }
        assert!(g.symbols.iter().find(|s| s.name == "A").unwrap().exported);
        assert!(!g.symbols.iter().find(|s| s.name == "B").unwrap().exported);
    }

    #[test]
    fn mod_declarations_become_file_edges() {
        let g = describe(
            "pub mod a;\nmod b;\npub(crate) mod c;\n#[path = \"alt/d.rs\"]\nmod d;\nfn _x() { include!(\"./table.rs\"); }\n",
        );
        // Same shape as aatxe-core::affected::extract_specifiers — `./{name}`
        // synthetic prefix so resolve_import can walk these.
        assert!(g.file_edges.iter().any(|s| s == "./a"));
        assert!(g.file_edges.iter().any(|s| s == "./b"));
        assert!(g.file_edges.iter().any(|s| s == "./c"));
        // `#[path = "alt/d.rs"]` overrides the synthetic `./d` with the
        // attribute body, prefix-normalised.
        assert!(
            g.file_edges.iter().any(|s| s == "./alt/d.rs"),
            "got: {:?}",
            g.file_edges
        );
        // include!() is file-local.
        assert!(g.file_edges.iter().any(|s| s == "./table.rs"));
        // Inline `mod foo { ... }` must NOT emit a file edge — it has a body.
        let g2 = describe("mod inline { pub fn x() {} }\n");
        assert!(g2.file_edges.is_empty(), "got: {:?}", g2.file_edges);
    }

    #[test]
    fn use_declarations_do_not_pollute_file_edges() {
        // `use` paths address symbols, not files — they must stay in
        // `imports` and stay out of `file_edges`, so the affected-set
        // resolver doesn't try to walk them as relative file specifiers.
        let g = describe("use crate::types::FileGraph;\nuse std::collections::HashMap;\n");
        assert!(g.file_edges.is_empty(), "got: {:?}", g.file_edges);
        assert!(!g.imports.is_empty());
    }

    #[test]
    fn imports_use_and_extern_crate() {
        let g = describe(
            "use crate::types::FileGraph;\nuse std::collections::HashMap;\nextern crate serde;\nfn main() {}\n",
        );
        assert!(g
            .imports
            .iter()
            .any(|s| s.contains("crate::types::FileGraph")));
        assert!(g
            .imports
            .iter()
            .any(|s| s.contains("std::collections::HashMap")));
        assert!(g.imports.iter().any(|s| s == "serde"));
    }

    #[test]
    fn call_edge_attributes_to_enclosing_fn() {
        let g = describe("fn callee() {}\nfn caller() { callee(); }\n");
        let e = g
            .edges
            .iter()
            .find(|e| e.to == "fn:callee" && e.kind == "call")
            .expect("call edge present");
        assert_eq!(e.from, "fn:caller");
    }

    #[test]
    fn control_flow_summary_counts_loops_and_conditionals() {
        let g = describe(
            "fn gnarly(xs: &[i32]) -> usize {\n  let mut n = 0;\n  for x in xs { if *x > 0 { n += 1; } }\n  while n > 100 { n -= 10; }\n  n\n}\n",
        );
        let s = g.symbols.iter().find(|s| s.name == "gnarly").unwrap();
        assert!(s.control_flow.contains("loops="), "got: {}", s.control_flow);
        assert!(s.control_flow.contains("conditionals="));
    }

    #[test]
    fn doc_comments_attach_when_immediately_preceding() {
        let g = describe("/// Renders the report.\n/// Multi-line.\nfn render() {}\n");
        let s = g.symbols.iter().find(|s| s.name == "render").unwrap();
        assert!(s.doc.contains("Renders the report"));
        assert!(s.doc.contains("Multi-line"));
    }

    #[test]
    fn empty_source_yields_empty_graph() {
        let g = describe("");
        assert!(g.symbols.is_empty());
        assert!(g.edges.is_empty());
        assert!(g.imports.is_empty());
    }
}
