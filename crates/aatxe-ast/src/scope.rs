//! Render a "Symbol scope (AST-derived)" markdown block for the council
//! proposer prompt.
//!
//! Takes a slice of `(path, FileGraph)` pairs (the workspace context)
//! plus the subset of paths that the PR actually touches and emits a
//! deterministic, compact block of:
//!
//! ```text
//! src/render.rs:
//!   - fn render (L12) [pub] `pub fn render(g: &FileGraph) -> String`
//!     loops=1 conditionals=2
//!     called by: src/cli.rs::main, src/lib.rs::format_block
//!   - fn fence (L48) `fn fence(body: &str) -> String`
//! ```
//!
//! "Called by" entries are line-resolved when the source describer
//! attributes the call to an enclosing function (every tree-sitter
//! describer does — `aatxe-ast::ts`, `aatxe-ast::go`, `aatxe-ast::rust_lang`).
//! When the regex fallback (`aatxe-ast::base`) is the only data source,
//! the entry falls back to file-only attribution (no `::name` suffix).
//! Both shapes coexist in the same list.
//!
//! The renderer is allocation-bounded by a soft byte cap so a large
//! workspace doesn't blow the prompt budget. When the cap is hit the
//! block is truncated with an explicit `… [+N more entries elided]`
//! marker so the model knows it isn't the whole picture.

use crate::types::{strip_id_prefix, FileGraph, LogicSymbol, SymbolKind};
use std::collections::HashMap;

/// One caller of a symbol, recorded for the renderer's "called by" line.
///
/// `path` is the file the call originates in. `caller` is the enclosing
/// function/method name when the source describer attributed the call to
/// one (every tree-sitter describer does); `None` when the call came from
/// the regex fallback's `file:<path>` placeholder, where line-resolved
/// attribution isn't available.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CallerEntry {
    path: String,
    caller: Option<String>,
}

impl CallerEntry {
    /// `path::caller` when caller-resolved, just `path` otherwise.
    fn rendered(&self) -> String {
        match &self.caller {
            Some(name) => format!("{}::{name}", self.path),
            None => self.path.clone(),
        }
    }
}

/// Hard cap on the rendered block. Council chunk policy already caps
/// related-file context at 256KB; AST scope is structural metadata —
/// keep it below 8KB so it never dominates the prompt.
pub const DEFAULT_MAX_BYTES: usize = 8 * 1024;

/// Render the scope block for the council prompt.
///
/// `workspace` is every parsed file in the review surface (changed +
/// related context). `changed_paths` is the subset whose symbols are
/// the actual focus of this PR — those files render in full, while
/// non-changed files only contribute caller-attribution data.
///
/// Returns `String::new()` when no symbols are extractable; the caller
/// (the council) checks `is_empty()` and skips the section header in
/// that case so the prompt doesn't get a stray empty block.
pub fn render_scope_block(workspace: &[(String, FileGraph)], changed_paths: &[String]) -> String {
    render_scope_block_with_cap(workspace, changed_paths, DEFAULT_MAX_BYTES)
}

/// Like [`render_scope_block`] but with a caller-specified byte cap.
/// Lets tests exercise the truncation path without 8KB fixtures.
pub(crate) fn render_scope_block_with_cap(
    workspace: &[(String, FileGraph)],
    changed_paths: &[String],
    cap: usize,
) -> String {
    if workspace.is_empty() || changed_paths.is_empty() {
        return String::new();
    }

    let callers = build_caller_index(workspace);

    let mut out = String::new();
    let mut elided: usize = 0;
    let changed_set: std::collections::HashSet<&str> =
        changed_paths.iter().map(String::as_str).collect();

    for (path, graph) in workspace {
        if !changed_set.contains(path.as_str()) {
            continue;
        }
        if graph.symbols.is_empty() {
            continue;
        }
        let header = format!("\n{path}:\n");
        if out.len() + header.len() > cap {
            elided += graph.symbols.len();
            continue;
        }
        out.push_str(&header);
        for sym in &graph.symbols {
            let entry = render_symbol_entry(path, sym, &callers);
            if out.len() + entry.len() > cap {
                elided += 1;
                continue;
            }
            out.push_str(&entry);
        }
    }

    if out.is_empty() {
        return String::new();
    }

    if elided > 0 {
        let marker = format!("\n… [+{elided} more entries elided to stay under {cap} bytes]\n");
        // Always make room for the marker even if it pushes us slightly
        // past cap — the truthful "elided" notice is worth ~80 bytes.
        out.push_str(&marker);
    }

    out
}

/// Build a `name → [CallerEntry]` index across the *entire* workspace so
/// a symbol's "called by" list is cross-file and (when the describer
/// supports it) line-resolved to the enclosing function.
///
/// Edge `from` shapes:
/// - `"fn:name"` / `"mtd:Owner.name"` / etc. — tree-sitter describer
///   resolved the call to an enclosing symbol; record `caller = Some(name)`.
/// - `"file:<path>"` — regex fallback's file-virtual-symbol; record
///   `caller = None` (the prefix-path is redundant with the iteration path).
/// - `"file"` — tree-sitter describer fell out of every enclosing symbol
///   (top-level expression); record `caller = None`.
fn build_caller_index(workspace: &[(String, FileGraph)]) -> HashMap<String, Vec<CallerEntry>> {
    let mut idx: HashMap<String, Vec<CallerEntry>> = HashMap::new();
    for (path, g) in workspace {
        for edge in &g.edges {
            if edge.kind != "call" {
                continue;
            }
            let callee_name = strip_id_prefix(&edge.to).to_string();
            let caller = if edge.from == "file" || edge.from.starts_with("file:") {
                None
            } else {
                Some(strip_id_prefix(&edge.from).to_string())
            };
            let entry = CallerEntry {
                path: path.to_string(),
                caller,
            };
            let bucket = idx.entry(callee_name).or_default();
            if !bucket.contains(&entry) {
                bucket.push(entry);
            }
        }
    }
    idx
}

fn render_symbol_entry(
    path: &str,
    sym: &LogicSymbol,
    callers: &HashMap<String, Vec<CallerEntry>>,
) -> String {
    let tag = sym.kind.tag();
    let exported = if sym.exported { " [pub]" } else { "" };
    let sig = if sym.signature.is_empty() {
        String::new()
    } else {
        format!(" `{}`", oneline(&sym.signature))
    };
    let mut s = format!(
        "  - {tag} {name} (L{line}){exported}{sig}\n",
        name = sym.name,
        line = sym.line,
    );
    if !sym.control_flow.is_empty() {
        s.push_str(&format!("    {}\n", sym.control_flow));
    }
    if !sym.doc.is_empty() {
        s.push_str(&format!("    doc: {}\n", oneline(&sym.doc)));
    }
    if let Some(by) = callers.get(&sym.name) {
        // Drop in-file self-recursion (`render` calling itself) so the
        // "called by" line surfaces only callers that genuinely add
        // context. Cross-file callers with the same name (a different
        // `render` in another file) and unrelated callers in the same
        // file are both kept.
        let others: Vec<String> = by
            .iter()
            .filter(|e| !(e.path == path && e.caller.as_deref() == Some(&sym.name)))
            .map(CallerEntry::rendered)
            .collect();
        if !others.is_empty() {
            s.push_str(&format!("    called by: {}\n", others.join(", ")));
        }
    }
    let _ = strip_id_prefix(&sym.id); // silence dead-code warning when feature-stripped
    let _ = SymbolKind::Function; // ditto
    s
}

fn oneline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for ch in s.chars() {
        match ch {
            '\n' | '\r' | '\t' => {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            ' ' => {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            c => {
                out.push(c);
                last_was_space = false;
            }
        }
    }
    if out.len() > 240 {
        out.truncate(237);
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LogicEdge, SymbolKind};

    fn sym(name: &str, kind: SymbolKind, line: usize, sig: &str, exported: bool) -> LogicSymbol {
        LogicSymbol {
            id: format!("{}:{}", kind.tag(), name),
            name: name.to_string(),
            kind,
            exported,
            line,
            signature: sig.to_string(),
            control_flow: String::new(),
            doc: String::new(),
        }
    }

    #[test]
    fn renders_changed_files_only() {
        let g1 = FileGraph {
            symbols: vec![sym(
                "render",
                SymbolKind::Function,
                12,
                "pub fn render() -> String",
                true,
            )],
            ..Default::default()
        };
        let g2 = FileGraph {
            symbols: vec![sym(
                "dont_render_me",
                SymbolKind::Function,
                4,
                "fn x()",
                false,
            )],
            ..Default::default()
        };
        let ws = vec![
            ("src/changed.rs".to_string(), g1),
            ("src/unrelated.rs".to_string(), g2),
        ];
        let out = render_scope_block(&ws, &["src/changed.rs".to_string()]);
        assert!(out.contains("src/changed.rs:"));
        assert!(out.contains("fn render"));
        assert!(out.contains("L12"));
        assert!(out.contains("[pub]"));
        assert!(out.contains("`pub fn render() -> String`"));
        // unrelated file's symbols MUST NOT appear.
        assert!(!out.contains("dont_render_me"));
        assert!(!out.contains("src/unrelated.rs"));
    }

    #[test]
    fn renders_called_by_when_cross_file_caller_exists_regex_fallback_shape() {
        // `from = "file:<path>"` is the regex extractor's shape — no
        // enclosing-symbol info, so the renderer falls back to file-only
        // attribution (no `::name` suffix).
        let defined = FileGraph {
            symbols: vec![sym("helper", SymbolKind::Function, 7, "fn helper()", false)],
            ..Default::default()
        };
        let caller = FileGraph {
            edges: vec![LogicEdge {
                from: "file:src/cli.rs".into(),
                to: "fn:helper".into(),
                kind: "call".into(),
            }],
            ..Default::default()
        };
        let ws = vec![
            ("src/defined.rs".to_string(), defined),
            ("src/cli.rs".to_string(), caller),
        ];
        let out = render_scope_block(&ws, &["src/defined.rs".to_string()]);
        assert!(
            out.contains("called by: src/cli.rs\n") || out.contains("called by: src/cli.rs,"),
            "regex-fallback shape should attribute file-only:\n{out}"
        );
        assert!(
            !out.contains("src/cli.rs::"),
            "regex fallback has no enclosing-symbol info, must NOT emit `::name`:\n{out}"
        );
    }

    #[test]
    fn line_resolved_caller_attribution_for_tree_sitter_edges() {
        // `from = "fn:main"` is the tree-sitter describers' shape —
        // line-resolved to the enclosing function. The renderer must
        // emit `path::caller_name` for these (the M2.2 / #7b promise).
        let defined = FileGraph {
            symbols: vec![sym("helper", SymbolKind::Function, 7, "fn helper()", false)],
            ..Default::default()
        };
        let caller = FileGraph {
            edges: vec![
                LogicEdge {
                    from: "fn:main".into(),
                    to: "fn:helper".into(),
                    kind: "call".into(),
                },
                LogicEdge {
                    from: "mtd:Cli.run".into(),
                    to: "fn:helper".into(),
                    kind: "call".into(),
                },
            ],
            ..Default::default()
        };
        let ws = vec![
            ("src/defined.rs".to_string(), defined),
            ("src/cli.rs".to_string(), caller),
        ];
        let out = render_scope_block(&ws, &["src/defined.rs".to_string()]);
        assert!(
            out.contains("called by: src/cli.rs::main, src/cli.rs::Cli.run")
                || out.contains("called by: src/cli.rs::Cli.run, src/cli.rs::main"),
            "tree-sitter edges should resolve caller name into `path::caller`:\n{out}"
        );
    }

    #[test]
    fn mixes_resolved_and_unresolved_callers_in_one_line() {
        // Real workspaces routinely have both — some files parsed by
        // tree-sitter, some by the regex fallback. Both shapes coexist
        // in the same `called by:` line, comma-separated, preserving
        // each shape's attribution semantics.
        let defined = FileGraph {
            symbols: vec![sym("helper", SymbolKind::Function, 3, "fn helper()", false)],
            ..Default::default()
        };
        let resolved_caller = FileGraph {
            edges: vec![LogicEdge {
                from: "fn:start".into(),
                to: "fn:helper".into(),
                kind: "call".into(),
            }],
            ..Default::default()
        };
        let unresolved_caller = FileGraph {
            edges: vec![LogicEdge {
                from: "file:src/legacy.py".into(),
                to: "fn:helper".into(),
                kind: "call".into(),
            }],
            ..Default::default()
        };
        let ws = vec![
            ("src/defined.rs".to_string(), defined),
            ("src/typed.rs".to_string(), resolved_caller),
            ("src/legacy.py".to_string(), unresolved_caller),
        ];
        let out = render_scope_block(&ws, &["src/defined.rs".to_string()]);
        assert!(
            out.contains("src/typed.rs::start"),
            "tree-sitter caller should be resolved:\n{out}"
        );
        assert!(
            out.contains("src/legacy.py"),
            "regex-fallback caller should still appear:\n{out}"
        );
        assert!(
            !out.contains("src/legacy.py::"),
            "regex-fallback must NOT gain a `::name` suffix:\n{out}"
        );
    }

    #[test]
    fn in_file_self_recursion_is_filtered_but_cross_file_homonym_is_kept() {
        // `render` in src/x.rs calls itself recursively (self-recursion):
        // suppress, otherwise every recursive helper would have a noisy
        // self-attribution. A `render` in src/y.rs that calls a `render`
        // in src/x.rs is a real cross-file edge — keep it.
        let defined = FileGraph {
            symbols: vec![sym("render", SymbolKind::Function, 4, "fn render()", false)],
            edges: vec![LogicEdge {
                from: "fn:render".into(),
                to: "fn:render".into(),
                kind: "call".into(),
            }],
            ..Default::default()
        };
        let homonym_caller = FileGraph {
            edges: vec![LogicEdge {
                from: "fn:render".into(),
                to: "fn:render".into(),
                kind: "call".into(),
            }],
            ..Default::default()
        };
        let ws = vec![
            ("src/x.rs".to_string(), defined),
            ("src/y.rs".to_string(), homonym_caller),
        ];
        let out = render_scope_block(&ws, &["src/x.rs".to_string()]);
        // Cross-file homonym appears.
        assert!(
            out.contains("called by: src/y.rs::render"),
            "cross-file homonym should appear:\n{out}"
        );
        // In-file self-recursion does NOT add `src/x.rs::render` to its
        // own "called by" line.
        assert!(
            !out.contains("src/x.rs::render"),
            "in-file self-recursion must be filtered:\n{out}"
        );
    }

    #[test]
    fn non_call_edge_kinds_are_ignored() {
        // The renderer only consumes `kind = "call"` — extends / implements
        // edges (if any describer grows them) must not pollute the
        // caller index.
        let defined = FileGraph {
            symbols: vec![sym("helper", SymbolKind::Function, 1, "fn helper()", false)],
            ..Default::default()
        };
        let caller = FileGraph {
            edges: vec![LogicEdge {
                from: "fn:main".into(),
                to: "fn:helper".into(),
                kind: "extends".into(),
            }],
            ..Default::default()
        };
        let ws = vec![
            ("src/defined.rs".to_string(), defined),
            ("src/cli.rs".to_string(), caller),
        ];
        let out = render_scope_block(&ws, &["src/defined.rs".to_string()]);
        assert!(
            !out.contains("called by:"),
            "non-call edges must not generate `called by` line:\n{out}"
        );
    }

    #[test]
    fn from_equals_bare_file_is_treated_as_unresolved() {
        // Tree-sitter describers emit `from = "file"` when no enclosing
        // symbol existed (top-level expression). That must render as
        // file-only attribution, same as the regex-fallback shape.
        let defined = FileGraph {
            symbols: vec![sym("helper", SymbolKind::Function, 1, "fn helper()", false)],
            ..Default::default()
        };
        let caller = FileGraph {
            edges: vec![LogicEdge {
                from: "file".into(),
                to: "fn:helper".into(),
                kind: "call".into(),
            }],
            ..Default::default()
        };
        let ws = vec![
            ("src/defined.rs".to_string(), defined),
            ("scripts/run.rs".to_string(), caller),
        ];
        let out = render_scope_block(&ws, &["src/defined.rs".to_string()]);
        assert!(
            out.contains("called by: scripts/run.rs\n"),
            "bare `file` from must produce file-only attribution:\n{out}"
        );
        assert!(
            !out.contains("scripts/run.rs::"),
            "bare `file` from must NOT produce `::name` suffix:\n{out}"
        );
    }

    #[test]
    fn empty_when_no_workspace_or_no_changed() {
        let g = FileGraph {
            symbols: vec![sym("a", SymbolKind::Function, 1, "fn a()", false)],
            ..Default::default()
        };
        assert!(render_scope_block(&[], &["x".to_string()]).is_empty());
        assert!(render_scope_block(&[("x".into(), g)], &[]).is_empty());
    }

    #[test]
    fn truncates_with_explicit_marker_when_cap_exceeded() {
        let mut symbols = Vec::new();
        for i in 0..50 {
            symbols.push(sym(
                &format!("sym_{i}"),
                SymbolKind::Function,
                i + 1,
                "fn x() -> ()",
                false,
            ));
        }
        let g = FileGraph {
            symbols,
            ..Default::default()
        };
        let ws = vec![("src/big.rs".to_string(), g)];
        // 256-byte cap forces truncation.
        let out = render_scope_block_with_cap(&ws, &["src/big.rs".to_string()], 256);
        assert!(
            out.contains("more entries elided"),
            "must emit elision marker:\n{out}"
        );
    }

    #[test]
    fn oneline_collapses_whitespace_and_truncates() {
        let s = oneline("a\nb\t  c");
        assert_eq!(s, "a b c");
        let long: String = (0..300).map(|_| 'x').collect();
        let one = oneline(&long);
        assert!(one.len() <= 240);
        assert!(one.ends_with('…'));
    }
}
