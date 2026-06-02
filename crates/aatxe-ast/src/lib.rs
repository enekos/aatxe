//! # aatxe-ast
//!
//! AST-based scope inspection so the aatxe council reasons about
//! symbol-level scope (callers, signatures, control flow, import graph)
//! instead of re-inferring it from flat diff text.
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
//!
//! ## Rendering for the council prompt
//!
//! After describing every file the council will see, build the prompt
//! block with [`scope::render_scope_block`]:
//!
//! ```ignore
//! let block = aatxe_ast::scope::render_scope_block(&graphs, &changed_paths);
//! // Pass `block` to `aatxe_council::CouncilOptions::ast_scope`.
//! ```

pub mod base;
pub mod describer;
pub mod scope;
pub mod types;

#[cfg(feature = "lang-go")]
mod go;
#[cfg(feature = "lang-rust")]
mod rust_lang;
#[cfg(feature = "lang-ts")]
mod ts;

pub use describer::{describe, language_for_path, LanguageDescriber, ParserPool};
pub use scope::render_scope_block;
pub use types::{FileGraph, LogicEdge, LogicSymbol, SymbolKind};

pub use aatxe_core::types::Language;
