//! Trait + dispatch for per-language describers, plus the parser pool.
//!
//! The trait is intentionally narrow: identity (`language_id`, file
//! extensions) and a single extract call. Production callers go through
//! the top-level [`describe`] convenience which dispatches by
//! [`Language`].

use crate::base::regex_extract;
use crate::types::FileGraph;
use aatxe_core::types::Language;

#[cfg(any(feature = "lang-ts", feature = "lang-go", feature = "lang-rust"))]
use std::sync::Mutex;
#[cfg(any(feature = "lang-ts", feature = "lang-go", feature = "lang-rust"))]
use tree_sitter::Parser;

/// What every describer must do: declare which extensions it owns and
/// parse a source string into a [`FileGraph`].
///
/// The interface is intentionally side-effect-free. Parsers, tree-sitter
/// state, scratch buffers — all of that is per-describer-impl business;
/// the council never sees it.
pub trait LanguageDescriber: Send + Sync {
    /// Short identifier (`"ts"`, `"go"`, `"rust"`, `"regex"`). Matched
    /// against [`Language`] variants in [`describe`].
    fn language_id(&self) -> &'static str;
    /// File extensions this describer claims (without the leading dot).
    fn extensions(&self) -> &'static [&'static str];
    /// Extract a [`FileGraph`] from `source`. `file_path` is used only
    /// for the `file_summary` field; the parser must not touch disk.
    fn extract_file_graph(&self, file_path: &str, source: &str) -> FileGraph;
}

/// Convenience: pick the right describer for a [`Language`] and parse.
///
/// Falls back to the regex extractor when the matching language feature
/// is compiled out, so the council degrades gracefully on partial-feature
/// builds instead of going scope-blind.
pub fn describe(lang: Language, file_path: &str, source: &str) -> FileGraph {
    #[cfg(feature = "lang-ts")]
    if lang == Language::Ts {
        return crate::ts::TsDescriber.extract_file_graph(file_path, source);
    }
    #[cfg(feature = "lang-go")]
    if lang == Language::Go {
        return crate::go::GoDescriber.extract_file_graph(file_path, source);
    }
    #[cfg(feature = "lang-rust")]
    if lang == Language::Rust {
        return crate::rust_lang::RustDescriber.extract_file_graph(file_path, source);
    }

    // Either the feature is off, or the lang variant isn't supported.
    let _ = lang;
    regex_extract(file_path, source)
}

/// Map a file path to an [`aatxe_core::types::Language`] by extension.
///
/// Returns `None` for anything outside the aatxe TS/Go/Rust universe so
/// the caller can decide whether to fall back to the regex extractor
/// (always succeeds) or skip the file entirely.
pub fn language_for_path(path: &str) -> Option<Language> {
    let ext = match path.rfind('.') {
        Some(i) => &path[i + 1..],
        None => return None,
    };
    // Bias to the matching language's `source_extensions` so we stay in
    // lockstep with `aatxe-core::affected`'s import resolver.
    if Language::Ts
        .source_extensions()
        .iter()
        .any(|e| *e == format!(".{ext}"))
    {
        return Some(Language::Ts);
    }
    if Language::Go
        .source_extensions()
        .iter()
        .any(|e| *e == format!(".{ext}"))
    {
        return Some(Language::Go);
    }
    if Language::Rust
        .source_extensions()
        .iter()
        .any(|e| *e == format!(".{ext}"))
    {
        return Some(Language::Rust);
    }
    None
}

/// Recyclable pool of tree-sitter parsers, mirroring mairu's
/// `parser_pool.go`. Parsers are expensive to allocate (~ms) — the pool
/// matters when describing hundreds of files in a single PR review.
///
/// Stub when every language feature is off; real implementation behind
/// any one feature so callers don't have to feature-gate their own code.
#[cfg(any(feature = "lang-ts", feature = "lang-go", feature = "lang-rust"))]
pub struct ParserPool {
    inner: Mutex<Vec<Parser>>,
}

#[cfg(any(feature = "lang-ts", feature = "lang-go", feature = "lang-rust"))]
impl Default for ParserPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(feature = "lang-ts", feature = "lang-go", feature = "lang-rust"))]
impl ParserPool {
    /// Empty pool. Parsers are lazy-allocated on first `take`.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
        }
    }

    /// Get a parser. Caller is responsible for `set_language` before
    /// using it — parsers are *language-agnostic* in the pool so we
    /// don't need three pools.
    pub fn take(&self) -> Parser {
        self.inner
            .lock()
            .ok()
            .and_then(|mut v| v.pop())
            .unwrap_or_default()
    }

    /// Return a parser to the pool. A poisoned mutex silently drops the
    /// parser; we'd rather leak one parser than panic in the council's
    /// hot path.
    pub fn give(&self, p: Parser) {
        if let Ok(mut v) = self.inner.lock() {
            v.push(p);
        }
    }
}

/// Stub when no language is compiled in — keeps the public name resolvable
/// and the (empty) impl trivially constructible.
#[cfg(not(any(feature = "lang-ts", feature = "lang-go", feature = "lang-rust")))]
pub struct ParserPool;

#[cfg(not(any(feature = "lang-ts", feature = "lang-go", feature = "lang-rust")))]
impl Default for ParserPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(any(feature = "lang-ts", feature = "lang-go", feature = "lang-rust")))]
impl ParserPool {
    pub fn new() -> Self {
        ParserPool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_for_path_matches_known_extensions() {
        assert_eq!(language_for_path("src/x.rs"), Some(Language::Rust));
        assert_eq!(language_for_path("src/x.ts"), Some(Language::Ts));
        assert_eq!(language_for_path("src/x.tsx"), Some(Language::Ts));
        assert_eq!(language_for_path("src/x.js"), Some(Language::Ts));
        assert_eq!(language_for_path("src/x.go"), Some(Language::Go));
        assert_eq!(language_for_path("Cargo.toml"), None);
        assert_eq!(language_for_path("README"), None);
    }

    #[test]
    fn describe_dispatches_or_falls_back_to_regex() {
        // The regex fallback is good enough to find a `fn` declaration —
        // we only need to know `describe` doesn't panic on any of the
        // three target languages and returns *some* symbols.
        let rs_src = "pub fn hello() {}\n";
        let g = describe(Language::Rust, "x.rs", rs_src);
        assert!(
            g.symbols.iter().any(|s| s.name == "hello"),
            "rust describe must surface `hello`"
        );

        let go_src = "package main\nfunc Hello() {}\n";
        let g = describe(Language::Go, "x.go", go_src);
        assert!(
            g.symbols.iter().any(|s| s.name == "Hello"),
            "go describe must surface `Hello`"
        );

        let ts_src = "export function hello() {}\n";
        let g = describe(Language::Ts, "x.ts", ts_src);
        assert!(
            g.symbols.iter().any(|s| s.name == "hello"),
            "ts describe must surface `hello`"
        );
    }
}
