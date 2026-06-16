//! CLI glue over the `aatxe-ast` crate.
//!
//! * [`ast_scope`] — builds the structural-metadata block injected into
//!   council proposer prompts.
//! * [`ast_import_extractor`] — tree-sitter-backed import graph feeding the
//!   `--affected` resolver.

pub mod ast_import_extractor;
pub mod ast_scope;
