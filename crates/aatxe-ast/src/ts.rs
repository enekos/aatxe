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
        "call_expression" => {
            // Capture inline arrow/function callbacks passed to
            // Router-style method calls — Express, Koa, Fastify, etc.
            // Without this branch, an
            //   `adminRouter.get("/x", requireAuth, async (req,res) => …)`
            // emits zero symbols because the arrow is buried inside a
            // `call_expression`'s arguments — no enclosing declaration
            // for the existing walker to attach to. Aatxe eval case
            // `security-authz-idor-export-route` (a real-world IDOR
            // fixture) is 0/3 in stub-mode precisely because of this
            // gap.
            //
            // Heuristic to avoid over-capture: only when the callee is
            // `<obj>.<method>` with `method` in the HTTP-verb set AND
            // the first argument is a string literal. That keeps
            // `Promise.all([…].map(async x => …))` and `arr.filter(x =>
            // …)` out of the scope index while reliably catching every
            // Express/Koa/Fastify route handler we've seen.
            //
            // We do NOT early-return: nested calls inside the arrow
            // body (e.g. `db.query(…)`) still need to be walked by the
            // default fall-through recursion at the bottom of the
            // function so the call-edge resolver picks them up.
            if let Some((method, path)) = router_method_call(node, bytes) {
                if let Some(args) = node.child_by_field_name("arguments") {
                    for arg in named_children(args) {
                        if matches!(
                            arg.kind(),
                            "arrow_function" | "function" | "function_expression"
                        ) {
                            out.push(LogicSymbol {
                                id: format!("route:{method}:{path}"),
                                name: format!("{method} {path}"),
                                kind: SymbolKind::Function,
                                exported: in_export,
                                line: arg.start_position().row + 1,
                                signature: signature_line(arg, source),
                                control_flow: control_flow_summary(arg),
                                doc: doc_comment_before(node, bytes),
                            });
                        }
                    }
                }
            }
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

/// If `node` is a `call_expression` whose function looks like
/// `<obj>.<method>(...)` with `<method>` in the HTTP-verb set and whose
/// first argument is a string literal, return the matched method name
/// (lowercased verbatim from the source) and the unquoted path.
///
/// Returns `None` for anything that doesn't fit the Router shape —
/// guarding against false positives like `Promise.all([...])`,
/// `arr.map(x => …)`, etc.
fn router_method_call(node: Node, bytes: &[u8]) -> Option<(String, String)> {
    let func = node.child_by_field_name("function")?;
    if func.kind() != "member_expression" {
        return None;
    }
    let prop = func.child_by_field_name("property")?;
    let method_text = prop.utf8_text(bytes).ok()?;
    if !is_router_method(method_text) {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    let first_arg = named_children(args).into_iter().next()?;
    if first_arg.kind() != "string" {
        return None;
    }
    let path_raw = first_arg.utf8_text(bytes).ok()?;
    let path = path_raw
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .to_string();
    if path.is_empty() {
        return None;
    }
    Some((method_text.to_string(), path))
}

fn is_router_method(s: &str) -> bool {
    matches!(
        s,
        "get" | "post" | "put" | "delete" | "patch" | "head" | "options" | "use" | "all"
    )
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

    // ----------------------- Router-method captures -----------------------
    //
    // The fixes below are calibrated to the real eval case
    // `security-authz-idor-export-route` — a multi-file Express IDOR
    // fixture whose admin.ts wires routes via `adminRouter.get(path,
    // middleware..., async (req, res) => …)`. Without the
    // `call_expression` branch in `walk`, the file emits zero symbols
    // (its only declaration is `export const adminRouter = Router();`,
    // a call-expression initializer that's not callable in the
    // describer's existing rules).

    #[test]
    fn router_get_with_middleware_and_arrow_callback_is_captured() {
        let g = describe(
            r#"
import { Router } from "express";
export const adminRouter = Router();

adminRouter.get("/users/:id/export", requireAuth, async (req, res) => {
  const user = await db.query("SELECT * FROM users WHERE id = $1", [req.params.id]);
  return res.json({ user });
});
"#,
        );
        let route = g
            .symbols
            .iter()
            .find(|s| s.id == "route:get:/users/:id/export")
            .expect("router.get arrow callback should be captured");
        assert_eq!(route.kind, SymbolKind::Function);
        assert_eq!(route.name, "get /users/:id/export");
        // The arrow function has no loops and one conditional? Let's
        // not over-specify control_flow; the field's there for
        // information, not gating. Just sanity-check signature looks
        // like an arrow.
        assert!(
            route.signature.contains("=>") || route.signature.contains("async"),
            "signature should resemble the arrow: {}",
            route.signature
        );
    }

    #[test]
    fn multiple_router_routes_emit_distinct_symbols() {
        // Two routes on the same router → two symbols with different
        // ids. The line numbers should match the arrow function's row.
        let g = describe(
            "adminRouter.get(\"/a\", async (req, res) => {});\n\
             adminRouter.post(\"/b\", (req, res) => {});\n",
        );
        let names: Vec<&str> = g.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"get /a"), "missing get /a, got {names:?}");
        assert!(names.contains(&"post /b"), "missing post /b, got {names:?}");
    }

    #[test]
    fn nested_call_expressions_dont_capture_non_route_arrows() {
        // The router branch must NOT swallow inline arrows passed to
        // generic methods like Promise.then / Array.map / .filter,
        // even when the receiver happens to be a router-like name.
        // Guarded by the "first arg is a string literal" check.
        let g = describe(
            "Promise.all([1,2,3].map(async (x) => x + 1)).then((xs) => xs);\n\
             arr.filter((x) => x > 0).map((x) => x * 2);\n",
        );
        for s in &g.symbols {
            assert!(
                !s.id.starts_with("route:"),
                "unexpected route symbol from non-router call: {s:?}"
            );
        }
    }

    #[test]
    fn router_use_with_string_and_arrow_is_captured() {
        // app.use("/api", (req, res, next) => { … }) is the standard
        // Express middleware shape and should be captured.
        let g = describe("app.use(\"/api\", (req, res, next) => { next(); });\n");
        assert!(g.symbols.iter().any(|s| s.id == "route:use:/api"));
    }

    #[test]
    fn router_use_without_string_arg_is_not_captured() {
        // app.use(middleware) — first arg is an identifier, not a
        // string. We treat this as "mounting a sub-router" and don't
        // emit a symbol (the sub-router's own routes are captured at
        // their definition site).
        let g = describe("app.use(adminRouter);\napp.use(requireAuth);\n");
        for s in &g.symbols {
            assert!(
                !s.id.starts_with("route:"),
                "unexpected route symbol: {s:?}"
            );
        }
    }

    #[test]
    fn router_get_with_no_callback_arg_emits_no_symbol() {
        // Just a string literal — no handler. We should NOT emit a
        // symbol because there's no callable to point the council at.
        let g = describe("adminRouter.get(\"/path\");\n");
        for s in &g.symbols {
            assert!(
                !s.id.starts_with("route:"),
                "should not emit a symbol when there's no arrow arg: {s:?}"
            );
        }
    }
}
