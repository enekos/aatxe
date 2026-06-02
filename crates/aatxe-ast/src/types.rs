//! Product types returned by every [`LanguageDescriber`].
//!
//! These are kept deliberately simple: a flat `Vec` of [`LogicSymbol`] +
//! a flat `Vec` of [`LogicEdge`] + a `Vec<String>` of import targets,
//! plus optional per-symbol natural-language descriptions. Symbol IDs
//! are stable strings (`fn:name`, `mtd:Class.name`, `cls:Name`,
//! `type:Name`) so edges can refer to them by string and the council
//! prompt can quote them verbatim.

use std::collections::HashMap;

/// Kind of a [`LogicSymbol`].
///
/// Stored as an explicit enum rather than the raw string mairu uses, so
/// the renderer and tests can match on it without re-parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    /// Top-level function (or const / let function expression in TS).
    Function,
    /// Method on a class / struct / impl block.
    Method,
    /// Class declaration (TS) or impl owner (Rust).
    Class,
    /// Type alias / interface / struct / enum.
    Type,
    /// Module-scope variable (TS `const`, Go `var`, Rust `static`/`const`).
    Variable,
}

impl SymbolKind {
    /// Short tag used in the rendered scope block ("fn", "mtd", "cls", "type", "var").
    pub fn tag(self) -> &'static str {
        match self {
            SymbolKind::Function => "fn",
            SymbolKind::Method => "mtd",
            SymbolKind::Class => "cls",
            SymbolKind::Type => "type",
            SymbolKind::Variable => "var",
        }
    }
}

/// Logical unit extracted from a file: name, kind, where it lives, and
/// its surface (signature + a tiny control-flow summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicSymbol {
    /// Stable ID, prefix matches [`SymbolKind::tag`]. E.g. `fn:render`,
    /// `mtd:DiffParser.parse`, `cls:DiffParser`.
    pub id: String,
    pub name: String,
    pub kind: SymbolKind,
    /// Whether the symbol is exported / public. For Rust `pub` items, for
    /// TS `export …`, for Go capitalised identifiers.
    pub exported: bool,
    /// 1-based line number where the symbol's declaration begins.
    pub line: usize,
    /// First-line signature, e.g. `fn render(graph: &FileGraph) -> String`.
    pub signature: String,
    /// Single-line tag with control-flow hints: `loops=2 conditionals=4`.
    /// Empty when no hints were extracted. Mairu uses a similar field as
    /// a cheap proxy for "is this function gnarly".
    pub control_flow: String,
    /// Doc comment immediately preceding the declaration, if any.
    /// Capped at 240 chars so the rendered scope block stays scannable.
    pub doc: String,
}

/// Edge between two [`LogicSymbol`]s. The intra-file call graph; the
/// council prompt uses these to flag "this changed function is called
/// from N other functions in the same file" without rolling up to
/// project-wide reachability.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LogicEdge {
    pub from: String,
    pub to: String,
    /// Edge kind. Today we only emit `"call"` but the type is open for
    /// `"override"`, `"extends"`, etc. when the per-lang describers grow.
    pub kind: String,
}

/// Result of parsing one source file.
///
/// `file_summary` is the one-liner the council prompt prints under the
/// file header. `raw_content` is the optional escape hatch when the
/// describer wants the raw source treated as the "content" (used by the
/// markdown describer in mairu — kept here for parity but unused by the
/// three coding-language describers today).
#[derive(Debug, Clone, Default)]
pub struct FileGraph {
    pub file_summary: String,
    pub raw_content: String,
    pub symbols: Vec<LogicSymbol>,
    pub edges: Vec<LogicEdge>,
    /// Verbatim import-target strings as written in the source
    /// (`"./util"`, `"crate::foo"`, `"github.com/x/y"`).
    pub imports: Vec<String>,
    pub symbol_descriptions: HashMap<String, String>,
}

impl FileGraph {
    /// Convenience: look up a symbol by ID. O(N) — the graphs are
    /// per-file so N is small (≤ a few hundred even for fat files).
    pub fn find_symbol(&self, id: &str) -> Option<&LogicSymbol> {
        self.symbols.iter().find(|s| s.id == id)
    }

    /// Names called from anywhere in the file (target side of every
    /// `call` edge, deduped). Used to compute cross-file callers when
    /// rendering scope across multiple [`FileGraph`]s.
    pub fn called_names(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for e in &self.edges {
            if e.kind == "call" {
                let n = strip_id_prefix(&e.to);
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out
    }
}

/// `"fn:foo"` → `"foo"`. Idempotent on unprefixed strings.
pub(crate) fn strip_id_prefix(id: &str) -> &str {
    id.find(':').map(|i| &id[i + 1..]).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_kind_tags_are_stable() {
        assert_eq!(SymbolKind::Function.tag(), "fn");
        assert_eq!(SymbolKind::Method.tag(), "mtd");
        assert_eq!(SymbolKind::Class.tag(), "cls");
        assert_eq!(SymbolKind::Type.tag(), "type");
        assert_eq!(SymbolKind::Variable.tag(), "var");
    }

    #[test]
    fn called_names_dedups_and_strips_prefix() {
        let g = FileGraph {
            edges: vec![
                LogicEdge {
                    from: "fn:a".into(),
                    to: "fn:b".into(),
                    kind: "call".into(),
                },
                LogicEdge {
                    from: "fn:a".into(),
                    to: "fn:b".into(),
                    kind: "call".into(),
                },
                LogicEdge {
                    from: "fn:c".into(),
                    to: "mtd:X.y".into(),
                    kind: "call".into(),
                },
                LogicEdge {
                    from: "fn:c".into(),
                    to: "fn:b".into(),
                    kind: "extends".into(),
                },
            ],
            ..Default::default()
        };
        let names = g.called_names();
        assert_eq!(names, vec!["b", "X.y"]);
    }

    #[test]
    fn find_symbol_returns_match_when_present() {
        let g = FileGraph {
            symbols: vec![LogicSymbol {
                id: "fn:render".into(),
                name: "render".into(),
                kind: SymbolKind::Function,
                exported: true,
                line: 12,
                signature: "fn render() -> String".into(),
                control_flow: String::new(),
                doc: String::new(),
            }],
            ..Default::default()
        };
        assert_eq!(g.find_symbol("fn:render").unwrap().line, 12);
        assert!(g.find_symbol("fn:missing").is_none());
    }
}
