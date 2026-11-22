//! # aatxe-ast
//!
//! Tree-sitter symbol and import extraction for TS / Go / Rust. The
//! import edges ([`FileGraph::file_edges`]) feed the `--affected`
//! resolver, which decides which benches a PR diff can touch.
//!
//! Pure crate: no IO, no globals, no parser-grammar state leakage. Parsing
//! takes `&str`, returns owned [`FileGraph`]. Tree-sitter grammars are
//! gated behind per-language Cargo features (`lang-ts`, `lang-go`,
//! `lang-rust`) so a downstream binary that only needs one language pays
//! for only one C grammar at build time. With all three features off the
//! crate still compiles and falls back to a regex extractor.
//!
//! The shape is lifted from mairu's `internal/ast/` package, adapted to
//! Rust idioms: a [`LanguageDescriber`] trait, a [`FileGraph`] product
//! type, a [`base::regex_extract`] fallback, and a pooled
//! [`describer::ParserPool`].
//!
//! ## Top-level usage
//!
//! ```ignore
//! use aatxe_ast::{describe, Language, FileGraph};
//!
//! let src = std::fs::read_to_string("src/lib.rs").unwrap();
//! let g: FileGraph = describe(Language::Rust, "src/lib.rs", &src);
//! for s in &g.symbols {
//!     println!("{}: {} @ {}", s.kind, s.name, s.line);
//! }
//! ```

pub mod base;
pub mod describer;
pub mod types;

#[cfg(feature = "lang-go")]
mod go;
#[cfg(feature = "lang-rust")]
mod rust_lang;
#[cfg(feature = "lang-ts")]
mod ts;

pub use describer::{describe, language_for_path, LanguageDescriber, ParserPool};
pub use types::{FileGraph, LogicEdge, LogicSymbol, SymbolKind};

pub use aatxe_core::types::Language;
