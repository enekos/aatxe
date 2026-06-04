//! Resolve the set of bench files affected by a diff.
//!
//! Aatxe walks each bench file's transitive **same-language** imports, then
//! intersects each bench's closure with the diff. A bench is "affected" iff
//! itself or any reachable file appears in the diff.
//!
//! Per-language import parsers come in two flavours: the in-crate regex
//! pass ([`RegexImportExtractor`], the default), and the AST-based
//! extractor in the CLI binary (`aatxe_ast::FileGraph::file_edges`),
//! plumbed through [`AffectedOptions::import_extractor`]. The AST pass
//! is language-correct (no string/comment false positives, captures
//! TS `export … from` re-exports + dynamic `import()` + Rust `mod foo;`
//! declarations) and stays in the CLI so `aatxe-core` keeps its
//! zero-dep, pure-logic shape. The regex path is the always-on fallback
//! the existing test surface still pins against. The escape hatch for
//! misses in either path is [`AffectedOptions::extra_changed_files`]
//! (e.g. when CI knows about a config change the parser can't see).
//!
//! ## Why three-dot diff
//!
//! `git diff $base...HEAD` answers "what did this branch change since it
//! diverged from base", which is exactly what CI is asking. It's stable
//! against the base moving forward independently.
//!
//! ## IO model
//!
//! All side-effecting calls (read a file, read a directory, run `git`) are
//! routed through trait objects. The default implementations use the real
//! filesystem and shell out to `git`; tests inject in-memory stand-ins.

use crate::types::Language;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Errors surfaced by [`resolve_affected`].
#[derive(Debug, thiserror::Error)]
pub enum AffectedError {
    #[error("not inside a git repository: {0}")]
    NotARepo(PathBuf),
    #[error("git command failed: {0}")]
    GitFailed(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Inputs and IO seams for [`resolve_affected`].
pub struct AffectedOptions<'a> {
    /// Project root (also the search root for bench discovery).
    pub cwd: PathBuf,
    /// Git ref to diff against, e.g. `origin/master`.
    pub base: String,
    pub language: Language,
    /// Bench-discovery globs. Empty ⇒ use [`Language::default_globs`].
    pub patterns: Vec<String>,
    /// Extra files to treat as changed. Escape hatch for tests and for cases
    /// the parser can't see (config files, codegen outputs, etc.).
    pub extra_changed_files: Vec<String>,
    pub git: &'a dyn GitRunner,
    pub fs: &'a dyn Fs,
    /// Per-language import extractor used to derive file edges from a
    /// source string. `None` ⇒ in-crate regex pass (the default, kept
    /// for backwards-compatibility with every existing call site).
    /// The CLI passes an AST-backed extractor that uses tree-sitter to
    /// extract the same shape language-correctly.
    pub import_extractor: Option<&'a dyn ImportExtractor>,
}

/// Pluggable source for "what file-edge specifiers does this source
/// contain". Implemented by [`RegexImportExtractor`] (the default) and,
/// in the CLI binary, by a wrapper around `aatxe_ast::describe(...)`.
///
/// The returned strings live in the same shape as [`extract_specifiers`]
/// (e.g. `./foo`, `./alt/d.rs`, `./shared`); [`resolve_import`] is what
/// turns each into one or more on-disk paths and is shared across both
/// implementations.
pub trait ImportExtractor {
    fn extract(&self, src: &str, lang: Language) -> Vec<String>;
}

/// The default in-crate extractor — delegates to [`extract_specifiers`].
///
/// Kept as a named type so the CLI can build an [`AffectedOptions`] that
/// re-uses the default without re-implementing the trait on the fly.
pub struct RegexImportExtractor;

impl ImportExtractor for RegexImportExtractor {
    fn extract(&self, src: &str, lang: Language) -> Vec<String> {
        extract_specifiers(src, lang)
    }
}

/// Output of [`resolve_affected`].
#[derive(Debug, Clone)]
pub struct AffectedSet {
    pub base: String,
    pub changed_files: Vec<String>,
    /// Absolute paths to bench files in the affected closure.
    pub bench_files: Vec<PathBuf>,
    /// Absolute paths to *every* discovered bench file. Callers diff this
    /// against `bench_files` to report what was skipped.
    pub all_bench_files: Vec<PathBuf>,
}

pub trait GitRunner {
    fn run(&self, args: &[&str], cwd: &Path) -> Result<String, AffectedError>;
}

pub trait Fs {
    fn read_to_string(&self, path: &Path) -> Result<String, AffectedError>;
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>, AffectedError>;
    fn metadata(&self, path: &Path) -> Result<EntryKind, AffectedError>;
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: PathBuf,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Other,
}

/// Resolve which bench files are affected by the diff between `base` and `HEAD`.
pub fn resolve_affected(opts: &AffectedOptions<'_>) -> Result<AffectedSet, AffectedError> {
    let all_bench_files = discover_benches(&opts.cwd, &opts.patterns, opts.language, opts.fs)?;
    if all_bench_files.is_empty() {
        return Ok(AffectedSet {
            base: opts.base.clone(),
            changed_files: vec![],
            bench_files: vec![],
            all_bench_files: vec![],
        });
    }

    let repo_root = detect_repo_root(&opts.cwd, opts.git)?;
    let mut changed_rel = git_changed_files(&repo_root, &opts.base, opts.git)?;
    for extra in &opts.extra_changed_files {
        changed_rel.push(extra.clone());
    }
    let changed_abs: HashSet<PathBuf> = changed_rel
        .iter()
        .map(|f| normalize_path(&repo_root.join(f)))
        .collect();

    let default_extractor = RegexImportExtractor;
    let extractor: &dyn ImportExtractor = match opts.import_extractor {
        Some(e) => e,
        None => &default_extractor,
    };

    let mut bench_files: Vec<PathBuf> = Vec::new();
    for bench in &all_bench_files {
        let reachable = collect_reachable_with(bench, opts.language, opts.fs, extractor);
        if reachable.iter().any(|f| changed_abs.contains(f)) {
            bench_files.push(bench.clone());
        }
    }

    Ok(AffectedSet {
        base: opts.base.clone(),
        changed_files: changed_rel,
        bench_files,
        all_bench_files,
    })
}

// --- discovery ---

fn discover_benches(
    cwd: &Path,
    patterns: &[String],
    lang: Language,
    fs: &dyn Fs,
) -> Result<Vec<PathBuf>, AffectedError> {
    let mut globs: Vec<&str> = patterns.iter().map(|s| s.as_str()).collect();
    if globs.is_empty() {
        globs.extend(lang.default_globs().iter().copied());
    }
    let matchers: Vec<GlobMatcher> = globs.iter().map(|g| GlobMatcher::new(g)).collect();
    let excludes = &["node_modules", "dist", "build", ".git", "target", "vendor"];
    let mut out: Vec<PathBuf> = Vec::new();
    walk(cwd, fs, &matchers, excludes, &mut out)?;
    out.sort();
    out.dedup();
    Ok(out)
}

fn walk(
    root: &Path,
    fs: &dyn Fs,
    matchers: &[GlobMatcher],
    excludes: &[&str],
    out: &mut Vec<PathBuf>,
) -> Result<(), AffectedError> {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs.read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries {
            let name = e.path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if excludes.iter().any(|ex| ex == &name) {
                continue;
            }
            match e.kind {
                EntryKind::Dir => stack.push(e.path),
                EntryKind::File => {
                    if matchers.iter().any(|m| m.matches(&e.path)) {
                        out.push(e.path);
                    }
                }
                EntryKind::Other => {}
            }
        }
    }
    Ok(())
}

// --- import graph ---

/// Walk imports/exports/uses transitively from `entry` and return every
/// reachable absolute path, using the default regex extractor.
///
/// Kept for backwards compatibility with the existing test surface and
/// the (few) external call sites. New code should call
/// [`collect_reachable_with`] and pass an explicit extractor — the CLI
/// uses that path to inject an AST-based extractor.
pub fn collect_reachable(entry: &Path, lang: Language, fs: &dyn Fs) -> HashSet<PathBuf> {
    collect_reachable_with(entry, lang, fs, &RegexImportExtractor)
}

/// Same as [`collect_reachable`], but with a caller-supplied
/// [`ImportExtractor`]. The CLI binary passes the AST-backed extractor;
/// callers without access to `aatxe-ast` (and the tests) pass
/// [`RegexImportExtractor`].
pub fn collect_reachable_with(
    entry: &Path,
    lang: Language,
    fs: &dyn Fs,
    extractor: &dyn ImportExtractor,
) -> HashSet<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = vec![normalize_path(entry)];
    while let Some(file) = stack.pop() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let src = match fs.read_to_string(&file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let specifiers = extractor.extract(&src, lang);
        let from_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        for spec in specifiers {
            if !is_relative_spec(&spec, lang) {
                continue;
            }
            for resolved in resolve_import(&from_dir, &spec, lang, fs) {
                if !seen.contains(&resolved) {
                    stack.push(resolved);
                }
            }
        }
    }
    seen
}

/// Extract import specifiers from source text.
///
/// **Deliberately permissive**: false positives (non-existent paths) get
/// filtered out by [`resolve_import`]; false negatives would lose real edges
/// in the graph and weaken the affected-set guarantee.
pub fn extract_specifiers(src: &str, lang: Language) -> Vec<String> {
    let stripped = strip_comments(src, lang);
    let mut out: Vec<String> = Vec::new();
    match lang {
        Language::Ts => {
            for caps in TS_FROM_RE.captures_iter(&stripped) {
                out.push(caps[1].to_string());
            }
            for caps in TS_SIDE_EFFECT_RE.captures_iter(&stripped) {
                out.push(caps[1].to_string());
            }
            for caps in TS_CALL_RE.captures_iter(&stripped) {
                out.push(caps[1].to_string());
            }
        }
        Language::Go => {
            // `import "path"` and `import ( ... )` blocks.
            for caps in GO_SINGLE_IMPORT_RE.captures_iter(&stripped) {
                out.push(caps[1].to_string());
            }
            for caps in GO_BLOCK_IMPORT_RE.captures_iter(&stripped) {
                let block = &caps[1];
                for sub in GO_BLOCK_PATH_RE.captures_iter(block) {
                    out.push(sub[1].to_string());
                }
            }
        }
        Language::Rust => {
            // We treat `mod foo;` declarations as edges: they reach `./foo.rs` or `./foo/mod.rs`.
            for caps in RUST_MOD_RE.captures_iter(&stripped) {
                out.push(format!("./{}", &caps[1]));
            }
            // `include!("path.rs")` macro — always file-local; normalise as relative.
            for caps in RUST_INCLUDE_RE.captures_iter(&stripped) {
                out.push(prefix_relative(&caps[1]));
            }
            // `path = "foo.rs"` attribute used to re-target a mod declaration.
            // The path is resolved relative to the *current* source file, so
            // we prefix `./` when the author didn't already supply `./`/`../`.
            for caps in RUST_PATH_ATTR_RE.captures_iter(&stripped) {
                out.push(prefix_relative(&caps[1]));
            }
        }
    }
    out
}

/// Prefix a bare path with `./` so [`is_relative_spec`] accepts it.
fn prefix_relative(p: &str) -> String {
    if p.starts_with("./") || p.starts_with("../") || p.starts_with('/') {
        p.to_string()
    } else {
        format!("./{p}")
    }
}

fn strip_comments(src: &str, lang: Language) -> String {
    match lang {
        Language::Ts | Language::Rust => {
            // `//` line comments and `/* ... */` block comments. Naive — not
            // string-literal-aware — which is fine: at worst we over-collect
            // a string that looks like an import, and the resolver drops it.
            let no_block = BLOCK_COMMENT_RE.replace_all(src, "").to_string();
            LINE_COMMENT_RE.replace_all(&no_block, "$1").to_string()
        }
        Language::Go => {
            // Go uses `//` and `/* */` like C.
            let no_block = BLOCK_COMMENT_RE.replace_all(src, "").to_string();
            LINE_COMMENT_RE.replace_all(&no_block, "$1").to_string()
        }
    }
}

fn is_relative_spec(spec: &str, lang: Language) -> bool {
    match lang {
        Language::Ts => {
            spec.starts_with("./")
                || spec.starts_with("../")
                || spec == "."
                || spec == ".."
                || spec.starts_with('/')
        }
        Language::Go => {
            // In Go, intra-module imports are tracked by module path, not
            // relative file path. Aatxe's import graph for Go therefore only
            // follows specifiers prefixed with `./` (an opt-in convention for
            // tests) — for a fully-resolved Go graph, the consumer should
            // supply `extra_changed_files` from `go list -deps`.
            spec.starts_with("./") || spec.starts_with("../")
        }
        Language::Rust => {
            // `mod` declarations are always relative to the current file.
            // The synthetic `./{name}` produced by [`extract_specifiers`]
            // always passes this check.
            spec.starts_with("./") || spec.starts_with("../")
        }
    }
}

/// Resolve a relative import to one or more on-disk paths.
///
/// * TS / Rust return at most one path (verbatim extension, candidate
///   extension, then `index.<ext>` / `mod.<ext>`).
/// * Go resolves a relative import to a *package* — the directory pointed
///   at by `spec` — and returns every `.go` file inside it, because Go
///   compiles all files of a package together.
pub fn resolve_import(from_dir: &Path, spec: &str, lang: Language, fs: &dyn Fs) -> Vec<PathBuf> {
    let exts = lang.source_extensions();
    let base = normalize_path(&from_dir.join(spec));

    // Verbatim extension on the spec itself.
    for ext in exts {
        if spec.ends_with(ext) {
            return if file_exists(&base, fs) {
                vec![base]
            } else {
                vec![]
            };
        }
    }

    // Go: directory-as-package — collect every `.go` file in `base`.
    if matches!(lang, Language::Go) {
        if let Ok(EntryKind::Dir) = fs.metadata(&base) {
            if let Ok(entries) = fs.read_dir(&base) {
                let mut out: Vec<PathBuf> = entries
                    .into_iter()
                    .filter(|e| matches!(e.kind, EntryKind::File))
                    .filter(|e| {
                        e.path
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s == "go")
                            .unwrap_or(false)
                    })
                    .map(|e| e.path)
                    .collect();
                out.sort();
                return out;
            }
        }
        // Fall through: occasionally a "./shared" specifier names a .go
        // file directly. Try the standard extension append below.
    }

    // Add candidate extension.
    for ext in exts {
        let cand = path_with_ext(&base, ext);
        if file_exists(&cand, fs) {
            return vec![cand];
        }
    }
    // Index / mod resolution (TS, Rust).
    if let Ok(EntryKind::Dir) = fs.metadata(&base) {
        let index_names: &[&str] = match lang {
            Language::Ts => &["index"],
            Language::Rust => &["mod"],
            Language::Go => &[],
        };
        for stem in index_names {
            for ext in exts {
                let cand = base.join(format!("{stem}{ext}"));
                if file_exists(&cand, fs) {
                    return vec![cand];
                }
            }
        }
    }
    vec![]
}

/// Collapse `.` and `..` components from a path without touching the
/// filesystem. Lets us compare paths from `Path::join` (which preserves
/// `./`) against canonical keys in the in-memory test FS.
fn normalize_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn path_with_ext(p: &Path, ext: &str) -> PathBuf {
    let mut s = p.to_path_buf().into_os_string();
    s.push(ext);
    PathBuf::from(s)
}

fn file_exists(p: &Path, fs: &dyn Fs) -> bool {
    matches!(fs.metadata(p), Ok(EntryKind::File))
}

// --- git ---

fn detect_repo_root(cwd: &Path, git: &dyn GitRunner) -> Result<PathBuf, AffectedError> {
    let out = git.run(&["rev-parse", "--show-toplevel"], cwd)?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Err(AffectedError::NotARepo(cwd.to_path_buf()));
    }
    Ok(PathBuf::from(trimmed))
}

fn git_changed_files(
    repo_root: &Path,
    base: &str,
    git: &dyn GitRunner,
) -> Result<Vec<String>, AffectedError> {
    let triple_dot = format!("{}...HEAD", base);
    let out = git.run(&["diff", "--name-only", &triple_dot], repo_root)?;
    let lines: Vec<String> = out
        .split('\n')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Ok(lines)
}

// --- glob ---

/// Minimal recursive-glob matcher: `**` matches across separators, `*` does
/// not. Sufficient for the bench-discovery patterns Aatxe supports.
#[derive(Debug)]
pub struct GlobMatcher {
    re: regex::Regex,
}

impl GlobMatcher {
    pub fn new(pattern: &str) -> Self {
        let mut rx = String::with_capacity(pattern.len() * 2);
        rx.push('^');
        let chars: Vec<char> = pattern.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if c == '*' && next == Some('*') {
                rx.push_str(".*");
                i += 2;
                if chars.get(i) == Some(&'/') {
                    i += 1;
                }
            } else if c == '*' {
                rx.push_str("[^/]*");
                i += 1;
            } else if c == '?' {
                rx.push_str("[^/]");
                i += 1;
            } else if matches!(
                c,
                '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\'
            ) {
                rx.push('\\');
                rx.push(c);
                i += 1;
            } else {
                rx.push(c);
                i += 1;
            }
        }
        rx.push('$');
        let re = regex::Regex::new(&rx).expect("internal: glob regex compile failed");
        Self { re }
    }
    pub fn matches(&self, p: &Path) -> bool {
        let s = p.to_string_lossy();
        self.re.is_match(&s)
    }
}

// --- regex statics ---

use once_cell::sync::Lazy;

static BLOCK_COMMENT_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?s)/\*.*?\*/").unwrap());
static LINE_COMMENT_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(^|[^:])//[^\n]*").unwrap());

static TS_FROM_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"\bfrom\s+['"]([^'"]+)['"]"#).unwrap());
static TS_SIDE_EFFECT_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"\bimport\s+['"]([^'"]+)['"]"#).unwrap());
static TS_CALL_RE: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(r#"\b(?:import|require)\s*\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap()
});

static GO_SINGLE_IMPORT_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"(?m)^\s*import\s+(?:[A-Za-z_]\w*\s+)?"([^"]+)""#).unwrap());
static GO_BLOCK_IMPORT_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"(?s)import\s*\(\s*(.*?)\s*\)"#).unwrap());
static GO_BLOCK_PATH_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"(?m)^\s*(?:[A-Za-z_]\w*\s+)?"([^"]+)""#).unwrap());

static RUST_MOD_RE: Lazy<regex::Regex> = Lazy::new(|| {
    // Matches: `mod x;` · `pub mod x;` · `pub(crate) mod x;` · `pub(in path) mod x;`.
    regex::Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*;").unwrap()
});
static RUST_INCLUDE_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"\binclude!\s*\(\s*"([^"]+)"\s*\)"#).unwrap());
static RUST_PATH_ATTR_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"#\[\s*path\s*=\s*"([^"]+)"\s*\]"#).unwrap());
