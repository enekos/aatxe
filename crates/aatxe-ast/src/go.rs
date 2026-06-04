//! Tree-sitter Go describer.
//!
//! Captures `func` declarations (both top-level and methods on a
//! receiver), `type` declarations (struct / interface / alias),
//! imports — and an intra-file call graph attributed to the
//! enclosing function. Exported symbols are detected by the
//! Go convention of a capitalised initial.

use crate::describer::LanguageDescriber;
use crate::types::{FileGraph, LogicEdge, LogicSymbol, SymbolKind};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Parser};

pub struct GoDescriber;

impl LanguageDescriber for GoDescriber {
    fn language_id(&self) -> &'static str {
        "go"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }
    fn extract_file_graph(&self, file_path: &str, source: &str) -> FileGraph {
        let mut parser = Parser::new();
        if parser.set_language(&tree_sitter_go::language()).is_err() {
            return crate::base::regex_extract(file_path, source);
        }
        let Some(tree) = parser.parse(source, None) else {
            return crate::base::regex_extract(file_path, source);
        };
        let bytes = source.as_bytes();
        let root = tree.root_node();

        let mut symbols: Vec<LogicSymbol> = Vec::new();
        let mut imports: Vec<String> = Vec::new();

        walk(root, bytes, source, &mut symbols, &mut imports);

        let edges = call_edges(&symbols, source);

        // For Go, every import path is also a candidate file edge —
        // `aatxe-core::affected::is_relative_spec` is what filters
        // module-path imports out at resolution time. Keep the lists in
        // lock-step so the AST extractor stays drop-in.
        let file_edges = imports.clone();

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
) {
    let kind = node.kind();
    match kind {
        "function_declaration" => {
            if let Some(name) = field_text(node, "name", bytes) {
                let exported = name.chars().next().is_some_and(|c| c.is_uppercase());
                out.push(LogicSymbol {
                    id: format!("fn:{name}"),
                    name: name.to_string(),
                    kind: SymbolKind::Function,
                    exported,
                    line: node.start_position().row + 1,
                    signature: signature_line(node, source),
                    control_flow: control_flow_summary(node),
                    doc: doc_comment_before(node, bytes),
                });
            }
        }
        "method_declaration" => {
            if let Some(name) = field_text(node, "name", bytes) {
                let exported = name.chars().next().is_some_and(|c| c.is_uppercase());
                let receiver = receiver_type(node, bytes).unwrap_or_else(|| "?".to_string());
                out.push(LogicSymbol {
                    id: format!("mtd:{receiver}.{name}"),
                    name: name.to_string(),
                    kind: SymbolKind::Method,
                    exported,
                    line: node.start_position().row + 1,
                    signature: signature_line(node, source),
                    control_flow: control_flow_summary(node),
                    doc: doc_comment_before(node, bytes),
                });
            }
        }
        "type_declaration" => {
            // `type Foo struct{…}` / `type Bar interface{…}` / `type Baz = X`.
            // The named children are `type_spec` nodes — one per name.
            for spec in named_children(node) {
                if spec.kind() != "type_spec" && spec.kind() != "type_alias" {
                    continue;
                }
                if let Some(name) = field_text(spec, "name", bytes) {
                    let exported = name.chars().next().is_some_and(|c| c.is_uppercase());
                    out.push(LogicSymbol {
                        id: format!("type:{name}"),
                        name: name.to_string(),
                        kind: SymbolKind::Type,
                        exported,
                        line: spec.start_position().row + 1,
                        signature: signature_line(spec, source),
                        control_flow: String::new(),
                        doc: doc_comment_before(node, bytes),
                    });
                }
            }
            return;
        }
        "import_declaration" => {
            // Walk all `interpreted_string_literal` descendants and pull
            // their text minus the quotes.
            let mut cursor = node.walk();
            for desc in node.children(&mut cursor) {
                collect_import_paths(desc, bytes, imports);
            }
            return;
        }
        _ => {}
    }
    for child in named_children(node) {
        walk(child, bytes, source, out, imports);
    }
}

fn collect_import_paths(node: Node, bytes: &[u8], out: &mut Vec<String>) {
    if node.kind() == "interpreted_string_literal" {
        let raw = node.utf8_text(bytes).unwrap_or("");
        let trimmed = raw.trim().trim_start_matches('"').trim_end_matches('"');
        if !trimmed.is_empty() && !out.iter().any(|s| s == trimmed) {
            out.push(trimmed.to_string());
        }
        return;
    }
    for child in named_children(node) {
        collect_import_paths(child, bytes, out);
    }
}

fn field_text<'a>(node: Node<'a>, field: &str, bytes: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(bytes).ok())
}

fn receiver_type(node: Node, bytes: &[u8]) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    // Walk to the first `type_identifier` underneath.
    let mut cursor = receiver.walk();
    let mut stack: Vec<Node> = receiver.children(&mut cursor).collect();
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" {
            return n.utf8_text(bytes).ok().map(str::to_string);
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    None
}

fn signature_line(node: Node, source: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    let slice = &source.as_bytes()[start..end.min(source.len())];
    let s = std::str::from_utf8(slice).unwrap_or("");
    let cut = s.find('{').unwrap_or(s.len());
    s[..cut].split_whitespace().collect::<Vec<_>>().join(" ")
}

fn control_flow_summary(node: Node) -> String {
    let (loops, conds) = count_loops_conds(node);
    if loops == 0 && conds == 0 {
        return String::new();
    }
    format!("loops={loops} conditionals={conds}")
}

fn count_loops_conds(node: Node) -> (usize, usize) {
    let mut loops = 0usize;
    let mut conds = 0usize;
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        match c.kind() {
            "for_statement" => loops += 1,
            "if_statement"
            | "type_switch_statement"
            | "expression_switch_statement"
            | "select_statement" => conds += 1,
            _ => {}
        }
        let (sl, sc) = count_loops_conds(c);
        loops += sl;
        conds += sc;
    }
    (loops, conds)
}

fn doc_comment_before(node: Node, bytes: &[u8]) -> String {
    let mut sib = node.prev_sibling();
    let mut collected: Vec<String> = Vec::new();
    while let Some(s) = sib {
        if s.kind() == "comment" {
            let txt = s.utf8_text(bytes).unwrap_or("");
            if txt.starts_with("//") {
                collected.push(strip_line_comment(txt));
                sib = s.prev_sibling();
                continue;
            }
        }
        break;
    }
    collected.reverse();
    let joined = collected.join(" ");
    if joined.len() > 240 {
        let mut t = joined;
        t.truncate(237);
        t.push('…');
        t
    } else {
        joined
    }
}

fn strip_line_comment(s: &str) -> String {
    s.lines()
        .map(|l| l.trim_start().trim_start_matches("//").trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn named_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn call_edges(symbols: &[LogicSymbol], source: &str) -> Vec<LogicEdge> {
    let mut ids_by_name: HashMap<&str, &str> = HashMap::new();
    for s in symbols {
        ids_by_name.entry(s.name.as_str()).or_insert(s.id.as_str());
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut edges: Vec<LogicEdge> = Vec::new();
    let call_re = regex::Regex::new(r"([A-Za-z_]\w*)\s*\(").expect("compile call_re");
    let mut sorted: Vec<&LogicSymbol> = symbols.iter().collect();
    sorted.sort_by_key(|s| s.line);
    for cap in call_re.captures_iter(source) {
        let m = cap.get(0).unwrap();
        let name = &cap[1];
        if matches!(
            name,
            "if" | "for"
                | "switch"
                | "func"
                | "return"
                | "go"
                | "defer"
                | "select"
                | "range"
                | "chan"
                | "var"
                | "const"
                | "type"
                | "package"
                | "import"
                | "map"
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
        GoDescriber.extract_file_graph("x.go", src)
    }

    #[test]
    fn top_level_func_extracted() {
        let g = describe("package main\nfunc Hello() string { return \"hi\" }\nfunc helper() {}\n");
        let h = g.symbols.iter().find(|s| s.name == "Hello").unwrap();
        assert!(h.exported, "capitalised => exported");
        assert_eq!(h.kind, SymbolKind::Function);
        let lo = g.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert!(!lo.exported);
    }

    #[test]
    fn method_gets_receiver_owner_prefix() {
        let g = describe(
            "package main\ntype User struct { Name string }\nfunc (u *User) Greet() string { return u.Name }\n",
        );
        let m = g.symbols.iter().find(|s| s.name == "Greet").unwrap();
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.id, "mtd:User.Greet");
        // Receiver type captured.
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "User" && s.kind == SymbolKind::Type));
    }

    #[test]
    fn type_struct_interface_alias() {
        let g =
            describe("package x\ntype Foo struct{}\ntype Bar interface{ Do() }\ntype Baz = Foo\n");
        for n in ["Foo", "Bar", "Baz"] {
            assert!(g.symbols.iter().any(|s| s.name == n), "missing {n}");
        }
    }

    #[test]
    fn imports_single_and_block_form() {
        let g = describe(
            "package x\nimport \"fmt\"\nimport (\n  \"strings\"\n  \"github.com/x/y\"\n)\n",
        );
        for want in ["fmt", "strings", "github.com/x/y"] {
            assert!(
                g.imports.iter().any(|s| s == want),
                "missing {want} in {:?}",
                g.imports
            );
        }
    }

    #[test]
    fn file_edges_mirror_imports_for_go() {
        // Go affected-resolution funnels every import through
        // `is_relative_spec`, so file_edges contains the same set as
        // `imports` — the resolver filters down to `./`/`../` itself.
        let g = describe(
            "package x\nimport \"fmt\"\nimport \"./shared\"\nimport (\n  \"github.com/x/y\"\n  \"../sibling\"\n)\n",
        );
        for want in ["fmt", "./shared", "github.com/x/y", "../sibling"] {
            assert!(
                g.file_edges.iter().any(|s| s == want),
                "missing {want} in file_edges {:?}",
                g.file_edges
            );
        }
    }

    #[test]
    fn call_edge_attribution() {
        let g = describe("package x\nfunc callee() {}\nfunc caller() { callee() }\n");
        let e = g.edges.iter().find(|e| e.to == "fn:callee").unwrap();
        assert_eq!(e.from, "fn:caller");
    }

    #[test]
    fn control_flow_summary_for_loops_and_if() {
        let g = describe(
            "package x\nfunc f(xs []int) int {\n  n := 0\n  for _, x := range xs { if x > 0 { n++ } }\n  return n\n}\n",
        );
        let s = g.symbols.iter().find(|s| s.name == "f").unwrap();
        assert!(s.control_flow.contains("loops="));
        assert!(s.control_flow.contains("conditionals="));
    }

    #[test]
    fn doc_comments_attach_when_immediately_preceding() {
        let g = describe("package x\n// Greet says hi.\n// Polite.\nfunc Greet() {}\n");
        let s = g.symbols.iter().find(|s| s.name == "Greet").unwrap();
        assert!(s.doc.contains("Greet says hi"));
    }
}
