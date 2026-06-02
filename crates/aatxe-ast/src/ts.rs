//! Tree-sitter TypeScript describer.
//!
//! Modelled on mairu's `treesitter_ts.go` (the heaviest reference at
//! 529 LOC) but trimmed to the symbol classes the council prompt
//! actually needs: `function` declarations, `class` declarations and
//! their `method_definition`s, top-level `const`/`let`/`var` bindings
//! whose initializer is an arrow function, and `import` targets. The
//! intra-file call graph attributes by enclosing function.
//!
//! Defaults to the **TypeScript** grammar; `.tsx` is handled by the
//! same describer but parsed with the `tsx` language variant so JSX
//! literals don't blow up the parse.

use crate::describer::LanguageDescriber;
use crate::types::{FileGraph, LogicEdge, LogicSymbol, SymbolKind};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Language, Node, Parser};

pub struct TsDescriber;

impl LanguageDescriber for TsDescriber {
    fn language_id(&self) -> &'static str {
        "ts"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "js", "jsx", "mjs", "cjs"]
    }
    fn extract_file_graph(&self, file_path: &str, source: &str) -> FileGraph {
        let lang: Language = if file_path.ends_with(".tsx") || file_path.ends_with(".jsx") {
            tree_sitter_typescript::language_tsx()
        } else {
            tree_sitter_typescript::language_typescript()
        };
        let mut parser = Parser::new();
        if parser.set_language(&lang).is_err() {
            return crate::base::regex_extract(file_path, source);
        }
        let Some(tree) = parser.parse(source, None) else {
            return crate::base::regex_extract(file_path, source);
        };
        let bytes = source.as_bytes();
        let root = tree.root_node();

        let mut symbols: Vec<LogicSymbol> = Vec::new();
        let mut imports: Vec<String> = Vec::new();

        walk(root, bytes, source, &mut symbols, &mut imports, None, false);

        let edges = call_edges(&symbols, source);

        FileGraph {
            file_summary: format!("{} symbols", symbols.len()),
            raw_content: String::new(),
            symbols,
            edges,
            imports,
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
    class_owner: Option<&str>,
    in_export: bool,
) {
    let kind = node.kind();
    match kind {
        "export_statement" => {
            // Recurse with `in_export=true`. Children of an export
            // statement inherit the export marker for `exported`.
            for child in named_children(node) {
                walk(child, bytes, source, out, imports, class_owner, true);
            }
            return;
        }
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name) = field_text(node, "name", bytes) {
                let outer = outer_decl_node(node);
                out.push(LogicSymbol {
                    id: format!("fn:{name}"),
                    name: name.to_string(),
                    kind: SymbolKind::Function,
                    exported: in_export,
                    line: outer.start_position().row + 1,
                    signature: signature_line(outer, source),
                    control_flow: control_flow_summary(node),
                    doc: doc_comment_before(outer, bytes),
                });
            }
        }
        "class_declaration" | "abstract_class_declaration" => {
            if let Some(name) = field_text(node, "name", bytes) {
                let owner = name.to_string();
                let outer = outer_decl_node(node);
                out.push(LogicSymbol {
                    id: format!("cls:{name}"),
                    name: name.to_string(),
                    kind: SymbolKind::Class,
                    exported: in_export,
                    line: outer.start_position().row + 1,
                    signature: signature_line(outer, source),
                    control_flow: String::new(),
                    doc: doc_comment_before(outer, bytes),
                });
                // Walk into the class body with `owner` set so methods
                // attribute correctly. Don't fall through.
                if let Some(body) = node.child_by_field_name("body") {
                    for child in named_children(body) {
                        walk(child, bytes, source, out, imports, Some(&owner), false);
                    }
                }
                return;
            }
        }
        "method_definition" => {
            if let Some(name) = field_text(node, "name", bytes) {
                let owner = class_owner.unwrap_or("?");
                out.push(LogicSymbol {
                    id: format!("mtd:{owner}.{name}"),
                    name: name.to_string(),
                    kind: SymbolKind::Method,
                    exported: in_export,
                    line: node.start_position().row + 1,
                    signature: signature_line(node, source),
                    control_flow: control_flow_summary(node),
                    doc: doc_comment_before(node, bytes),
                });
            }
        }
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
            if let Some(name) = field_text(node, "name", bytes) {
                let outer = outer_decl_node(node);
                out.push(LogicSymbol {
                    id: format!("type:{name}"),
                    name: name.to_string(),
                    kind: SymbolKind::Type,
                    exported: in_export,
                    line: outer.start_position().row + 1,
                    signature: signature_line(outer, source),
                    control_flow: String::new(),
                    doc: doc_comment_before(outer, bytes),
                });
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            // `const foo = (x: T) => …` — capture as a Function symbol
            // when the initializer is an arrow/function expression. Skip
            // plain `const PI = 3.14`.
            for d in named_children(node) {
                if d.kind() != "variable_declarator" {
                    continue;
                }
                let name = match field_text(d, "name", bytes) {
                    Some(n) => n,
                    None => continue,
                };
                let val = match d.child_by_field_name("value") {
                    Some(v) => v,
                    None => continue,
                };
                let is_callable = matches!(
                    val.kind(),
                    "arrow_function" | "function" | "function_expression"
                );
                let kind = if is_callable {
                    SymbolKind::Function
                } else {
                    SymbolKind::Variable
                };
                // Skip non-callable vars at non-top-level — they're noise.
                if !is_callable {
                    continue;
                }
                out.push(LogicSymbol {
                    id: format!("{}:{}", kind.tag(), name),
                    name: name.to_string(),
                    kind,
                    exported: in_export,
                    line: d.start_position().row + 1,
                    signature: signature_line(d, source),
                    control_flow: control_flow_summary(val),
                    doc: doc_comment_before(node, bytes),
                });
            }
            return;
        }
        "import_statement" => {
            if let Some(src) = field_text(node, "source", bytes) {
                let trimmed = src.trim().trim_start_matches('"').trim_end_matches('"');
                let trimmed = trimmed.trim_start_matches('\'').trim_end_matches('\'');
                if !trimmed.is_empty() && !imports.iter().any(|s| s == trimmed) {
                    imports.push(trimmed.to_string());
                }
            }
            return;
        }
        _ => {}
    }
    for child in named_children(node) {
        walk(child, bytes, source, out, imports, class_owner, in_export);
    }
}

fn field_text<'a>(node: Node<'a>, field: &str, bytes: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(bytes).ok())
}

/// If `node`'s parent is an `export_statement`, return the parent so the
/// caller picks up the leading `export ` keyword in signature slicing
/// and looks for JSDoc above the export, not the inner declaration.
fn outer_decl_node(node: Node) -> Node {
    if let Some(p) = node.parent() {
        if p.kind() == "export_statement" {
            return p;
        }
    }
    node
}

fn signature_line(node: Node, source: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    let slice = &source.as_bytes()[start..end.min(source.len())];
    let s = std::str::from_utf8(slice).unwrap_or("");
    let cut_brace = s.find('{').unwrap_or(s.len());
    let cut_arrow = s.find("=>").map(|i| i + 2).unwrap_or(s.len());
    let cut = cut_brace.min(cut_arrow);
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
            "for_statement" | "for_in_statement" | "for_of_statement" | "while_statement"
            | "do_statement" => loops += 1,
            "if_statement" | "switch_statement" | "ternary_expression" => conds += 1,
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
            if txt.starts_with("/**") || txt.starts_with("//") {
                collected.push(strip_comment_markers(txt));
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

fn strip_comment_markers(s: &str) -> String {
    s.lines()
        .map(|l| {
            l.trim_start()
                .trim_start_matches("/**")
                .trim_end_matches("*/")
                .trim_start_matches("//")
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

fn call_edges(symbols: &[LogicSymbol], source: &str) -> Vec<LogicEdge> {
    let mut ids_by_name: HashMap<&str, &str> = HashMap::new();
    for s in symbols {
        ids_by_name.entry(s.name.as_str()).or_insert(s.id.as_str());
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut edges: Vec<LogicEdge> = Vec::new();
    let call_re = regex::Regex::new(r"([A-Za-z_$][\w$]*)\s*\(").expect("compile call_re");
    let mut sorted: Vec<&LogicSymbol> = symbols.iter().collect();
    sorted.sort_by_key(|s| s.line);
    for cap in call_re.captures_iter(source) {
        let m = cap.get(0).unwrap();
        let name = &cap[1];
        if matches!(
            name,
            "if" | "for"
                | "while"
                | "switch"
                | "function"
                | "return"
                | "catch"
                | "do"
                | "throw"
                | "typeof"
                | "new"
                | "await"
                | "async"
                | "yield"
                | "case"
                | "else"
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
        TsDescriber.extract_file_graph("x.ts", src)
    }

    #[test]
    fn function_declaration_with_export() {
        let g = describe("export function render(): string { return ''; }\n");
        let s = g.symbols.iter().find(|s| s.name == "render").unwrap();
        assert!(s.exported, "`export function` => exported");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(s.signature.contains("export function render"));
    }

    #[test]
    fn class_with_methods_owner_prefix() {
        let g = describe(
            "export class DiffParser {\n  parse(input: string): void {}\n  private helper(): void {}\n}\n",
        );
        assert!(g
            .symbols
            .iter()
            .any(|s| s.name == "DiffParser" && s.kind == SymbolKind::Class && s.exported));
        let parse = g.symbols.iter().find(|s| s.name == "parse").unwrap();
        assert_eq!(parse.kind, SymbolKind::Method);
        assert_eq!(parse.id, "mtd:DiffParser.parse");
        // Private/public modifiers don't drive `exported`; only top-level
        // `export` does (the class is exported, but methods aren't
        // independently re-exported).
        assert!(!parse.exported);
    }

    #[test]
    fn arrow_function_const_captured_as_function() {
        let g = describe("export const render = (g: FileGraph): string => '';\n");
        let s = g.symbols.iter().find(|s| s.name == "render").unwrap();
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(s.exported);
    }

    #[test]
    fn imports_collected_from_paths() {
        let g =
            describe("import { a } from './util';\nimport b from 'pkg';\nimport 'side-effect';\n");
        for want in ["./util", "pkg", "side-effect"] {
            assert!(g.imports.iter().any(|s| s == want), "missing {want}");
        }
    }

    #[test]
    fn interface_and_type_alias_emit_type_kind() {
        let g = describe("export interface User { id: number }\nexport type ID = number;\n");
        let u = g.symbols.iter().find(|s| s.name == "User").unwrap();
        let id = g.symbols.iter().find(|s| s.name == "ID").unwrap();
        assert_eq!(u.kind, SymbolKind::Type);
        assert_eq!(id.kind, SymbolKind::Type);
        assert!(u.exported && id.exported);
    }

    #[test]
    fn call_edge_attribution() {
        let g = describe("function callee() {}\nfunction caller() { callee(); }\n");
        let e = g.edges.iter().find(|e| e.to == "fn:callee").unwrap();
        assert_eq!(e.from, "fn:caller");
    }

    #[test]
    fn tsx_grammar_parses_jsx() {
        // Sanity: TSX path should not fall back to regex extractor on
        // JSX content. The describer detects `.tsx` and uses TSX grammar.
        let g = TsDescriber
            .extract_file_graph("x.tsx", "export function App() { return <div>hi</div>; }\n");
        assert!(g.symbols.iter().any(|s| s.name == "App"));
    }

    #[test]
    fn jsdoc_attaches_to_following_declaration() {
        let g = describe("/** Renders the report. */\nexport function render(): void {}\n");
        let s = g.symbols.iter().find(|s| s.name == "render").unwrap();
        assert!(s.doc.contains("Renders the report"));
    }
}
