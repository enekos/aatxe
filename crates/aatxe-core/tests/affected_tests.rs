//! Affected-set — in-memory FS + git tests, one per supported language.

use aatxe_core::affected::{
    collect_reachable, extract_specifiers, resolve_affected, resolve_import, AffectedError,
    AffectedOptions, DirEntry, EntryKind, Fs, GitRunner, GlobMatcher,
};
use aatxe_core::types::Language;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// In-memory filesystem for tests. Paths are absolute, POSIX-style.
#[derive(Default)]
struct MemFs {
    files: HashMap<PathBuf, String>,
    dirs: HashMap<PathBuf, Vec<PathBuf>>,
}

impl MemFs {
    fn file(mut self, p: &str, body: &str) -> Self {
        let path = PathBuf::from(p);
        if let Some(parent) = path.parent() {
            // Register every ancestor directory and link it to its parent so
            // a recursive walk from the root reaches every leaf.
            let mut prev: Option<PathBuf> = None;
            let mut cur: PathBuf = PathBuf::new();
            for comp in parent.components() {
                cur.push(comp.as_os_str());
                self.dirs.entry(cur.clone()).or_default();
                if let Some(p) = prev.as_ref() {
                    let children = self.dirs.entry(p.clone()).or_default();
                    if !children.contains(&cur) {
                        children.push(cur.clone());
                    }
                }
                prev = Some(cur.clone());
            }
            let children = self.dirs.entry(parent.to_path_buf()).or_default();
            if !children.contains(&path) {
                children.push(path.clone());
            }
        }
        self.files.insert(path, body.to_string());
        self
    }
}

impl Fs for MemFs {
    fn read_to_string(&self, path: &Path) -> Result<String, AffectedError> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| AffectedError::Io(format!("not found: {path:?}")))
    }
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>, AffectedError> {
        let children = self
            .dirs
            .get(path)
            .ok_or_else(|| AffectedError::Io(format!("no such dir: {path:?}")))?;
        let mut out: Vec<DirEntry> = Vec::with_capacity(children.len());
        for c in children {
            let kind = if self.files.contains_key(c) {
                EntryKind::File
            } else if self.dirs.contains_key(c) {
                EntryKind::Dir
            } else {
                EntryKind::Other
            };
            out.push(DirEntry {
                path: c.clone(),
                kind,
            });
        }
        Ok(out)
    }
    fn metadata(&self, path: &Path) -> Result<EntryKind, AffectedError> {
        if self.files.contains_key(path) {
            Ok(EntryKind::File)
        } else if self.dirs.contains_key(path) {
            Ok(EntryKind::Dir)
        } else {
            Err(AffectedError::Io(format!("missing: {path:?}")))
        }
    }
}

struct FixedGit {
    root: PathBuf,
    changed: Vec<String>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl GitRunner for FixedGit {
    fn run(&self, args: &[&str], _cwd: &Path) -> Result<String, AffectedError> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        match args.first().copied() {
            Some("rev-parse") => Ok(format!("{}\n", self.root.display())),
            Some("diff") => Ok(self.changed.join("\n")),
            _ => Err(AffectedError::GitFailed("unexpected git call".to_string())),
        }
    }
}

// --- import extractor seam ---

#[test]
fn custom_import_extractor_is_consulted_instead_of_regex() {
    // The new `ImportExtractor` seam (introduced by #7a so the CLI can
    // swap in an AST-backed extractor) must completely replace the
    // regex pass when the caller supplies one. Verify it both ways:
    //   1. a custom extractor returning an empty Vec ⇒ no edges
    //      followed, so foo.bench.ts is NOT affected even though it
    //      imports foo.ts (which IS changed);
    //   2. an extractor that returns the right specifier ⇒ edge walked,
    //      foo.bench.ts becomes affected.
    use aatxe_core::affected::ImportExtractor;
    use aatxe_core::types::Language;

    struct Empty;
    impl ImportExtractor for Empty {
        fn extract(&self, _src: &str, _lang: Language) -> Vec<String> {
            vec![]
        }
    }
    struct Hardcoded(Vec<String>);
    impl ImportExtractor for Hardcoded {
        fn extract(&self, _src: &str, _lang: Language) -> Vec<String> {
            self.0.clone()
        }
    }

    let fs = MemFs::default()
        .file("/r/svc/sub/foo.ts", "export const x = 1")
        .file(
            "/r/svc/sub/foo.bench.ts",
            "import { x } from './foo'\nconst _ = x",
        );
    let git = FixedGit {
        root: PathBuf::from("/r"),
        changed: vec!["svc/sub/foo.ts".into()],
        calls: RefCell::new(vec![]),
    };

    // Case 1: empty extractor — no edges, bench not affected.
    let empty = Empty;
    let set = resolve_affected(&AffectedOptions {
        cwd: PathBuf::from("/r/svc"),
        base: "origin/master".into(),
        language: Language::Ts,
        patterns: vec![],
        extra_changed_files: vec![],
        git: &git,
        fs: &fs,
        import_extractor: Some(&empty),
    })
    .unwrap();
    assert!(
        set.bench_files.is_empty(),
        "empty extractor must short-circuit the affected walk; got {:?}",
        set.bench_files
    );

    // Case 2: hardcoded ./foo specifier — edge resolved, bench affected.
    let hardcoded = Hardcoded(vec!["./foo".to_string()]);
    let set = resolve_affected(&AffectedOptions {
        cwd: PathBuf::from("/r/svc"),
        base: "origin/master".into(),
        language: Language::Ts,
        patterns: vec![],
        extra_changed_files: vec![],
        git: &FixedGit {
            root: PathBuf::from("/r"),
            changed: vec!["svc/sub/foo.ts".into()],
            calls: RefCell::new(vec![]),
        },
        fs: &fs,
        import_extractor: Some(&hardcoded),
    })
    .unwrap();
    let names: Vec<&str> = set
        .bench_files
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(names, vec!["foo.bench.ts"]);
}

// --- ts ---

#[test]
fn ts_import_specifiers_extracted() {
    let src = r#"
        import { x } from './x'
        import './side-effect'
        import * as y from './y.ts'
        export { z } from '../z'
        const m = require('./m')
        const d = import('./d')
        // import './commented' should be ignored
        /* import './also-commented' */
    "#;
    let specs = extract_specifiers(src, Language::Ts);
    assert!(specs.contains(&"./x".to_string()));
    assert!(specs.contains(&"./side-effect".to_string()));
    assert!(specs.contains(&"./y.ts".to_string()));
    assert!(specs.contains(&"../z".to_string()));
    assert!(specs.contains(&"./m".to_string()));
    assert!(specs.contains(&"./d".to_string()));
    assert!(!specs.iter().any(|s| s.contains("commented")));
}

#[test]
fn ts_resolves_index_and_extension_variants() {
    let fs = MemFs::default()
        .file("/r/a.ts", "import { b } from './b'")
        .file("/r/b.ts", "")
        .file("/r/c/index.ts", "export const c = 1")
        .file("/r/d/index.js", "module.exports = 1");
    assert_eq!(
        resolve_import(Path::new("/r"), "./b", Language::Ts, &fs),
        vec![PathBuf::from("/r/b.ts")]
    );
    assert_eq!(
        resolve_import(Path::new("/r"), "./c", Language::Ts, &fs),
        vec![PathBuf::from("/r/c/index.ts")]
    );
    assert_eq!(
        resolve_import(Path::new("/r"), "./d", Language::Ts, &fs),
        vec![PathBuf::from("/r/d/index.js")]
    );
    assert!(resolve_import(Path::new("/r"), "./missing", Language::Ts, &fs).is_empty());
}

#[test]
fn ts_affected_intersects_diff_with_reachable_set() {
    let fs = MemFs::default()
        .file("/r/svc/sub/foo.ts", "export const x = 1")
        .file(
            "/r/svc/sub/foo.bench.ts",
            "import { x } from './foo'\nconst _ = x",
        )
        .file("/r/svc/other.bench.ts", "export const y = 2");
    let git = FixedGit {
        root: PathBuf::from("/r"),
        changed: vec!["svc/sub/foo.ts".into()],
        calls: RefCell::new(vec![]),
    };
    let set = resolve_affected(&AffectedOptions {
        cwd: PathBuf::from("/r/svc"),
        base: "origin/master".into(),
        language: Language::Ts,
        patterns: vec![],
        extra_changed_files: vec![],
        git: &git,
        fs: &fs,
        import_extractor: None,
    })
    .unwrap();
    let names: Vec<&str> = set
        .bench_files
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(names, vec!["foo.bench.ts"]);
    assert_eq!(
        set.all_bench_files.len(),
        2,
        "discovery should still see both"
    );
}

// --- go ---

#[test]
fn go_import_specifiers_extracted() {
    let src = r#"
        package x
        import "fmt"
        import alias "github.com/foo/bar"
        import (
            "io"
            stdfmt "fmt"
            "./local"
        )
    "#;
    let specs = extract_specifiers(src, Language::Go);
    assert!(specs.contains(&"fmt".to_string()));
    assert!(specs.contains(&"github.com/foo/bar".to_string()));
    assert!(specs.contains(&"io".to_string()));
    assert!(specs.contains(&"./local".to_string()));
}

#[test]
fn go_affected_with_explicit_changed_files() {
    let fs = MemFs::default()
        .file("/r/svc/parser.go", "package svc")
        .file(
            "/r/svc/parser_bench_test.go",
            r#"package svc
            import "./shared"
            func _() { _ = shared.X }
            "#,
        )
        .file("/r/svc/shared/shared.go", "package shared\nconst X = 1")
        .file("/r/svc/shared.go", "package svc"); // unrelated
    let git = FixedGit {
        root: PathBuf::from("/r"),
        changed: vec![],
        calls: RefCell::new(vec![]),
    };
    let set = resolve_affected(&AffectedOptions {
        cwd: PathBuf::from("/r/svc"),
        base: "origin/master".into(),
        language: Language::Go,
        patterns: vec![],
        // CI knows about a config change git didn't surface.
        extra_changed_files: vec!["svc/shared/shared.go".into()],
        git: &git,
        fs: &fs,
        import_extractor: None,
    })
    .unwrap();
    assert_eq!(set.bench_files.len(), 1);
    assert_eq!(
        set.bench_files[0].file_name().unwrap(),
        "parser_bench_test.go"
    );
}

// --- rust ---

#[test]
fn rust_mod_declarations_are_extracted() {
    let src = r#"
        pub mod a;
        mod b;
        pub(crate) mod c;
        #[path = "alt/d.rs"]
        mod d;
        fn _x() { include!("./table.rs"); }
    "#;
    let specs = extract_specifiers(src, Language::Rust);
    assert!(specs.iter().any(|s| s == "./a"));
    assert!(specs.iter().any(|s| s == "./b"));
    assert!(specs.iter().any(|s| s == "./c"));
    // Bare `path = "alt/d.rs"` is normalised to `./alt/d.rs` so it round-trips
    // through `is_relative_spec` and `resolve_import`.
    assert!(specs.iter().any(|s| s == "./alt/d.rs"));
    assert!(specs.iter().any(|s| s == "./table.rs"));
}

#[test]
fn rust_collect_reachable_walks_mod_tree() {
    let fs = MemFs::default()
        .file(
            "/r/benches/x.rs",
            "mod helpers;\nfn main() { helpers::go(); }",
        )
        .file("/r/benches/helpers.rs", "pub fn go() {}");
    let set = collect_reachable(Path::new("/r/benches/x.rs"), Language::Rust, &fs);
    let mut have: Vec<&str> = set.iter().filter_map(|p| p.to_str()).collect();
    have.sort();
    assert!(have.contains(&"/r/benches/x.rs"));
    assert!(have.contains(&"/r/benches/helpers.rs"));
}

// --- glob ---

#[test]
fn glob_double_star_crosses_separators() {
    let m = GlobMatcher::new("**/*.bench.ts");
    assert!(m.matches(Path::new("/r/svc/a.bench.ts")));
    assert!(m.matches(Path::new("/r/svc/sub/a.bench.ts")));
    assert!(!m.matches(Path::new("/r/svc/a.ts")));
}

#[test]
fn glob_single_star_does_not_cross_separators() {
    let m = GlobMatcher::new("/r/*.bench.go");
    assert!(m.matches(Path::new("/r/a.bench.go")));
    assert!(!m.matches(Path::new("/r/sub/a.bench.go")));
}

// --- graph traversal edge cases ---

#[test]
fn collect_reachable_handles_cycles() {
    // a → b → a must not loop forever.
    let fs = MemFs::default()
        .file("/r/a.ts", "import './b'")
        .file("/r/b.ts", "import './a'");
    let set = collect_reachable(Path::new("/r/a.ts"), Language::Ts, &fs);
    assert_eq!(set.len(), 2);
    assert!(set.iter().any(|p| p.ends_with("a.ts")));
    assert!(set.iter().any(|p| p.ends_with("b.ts")));
}

#[test]
fn collect_reachable_ignores_non_relative_imports() {
    // `react` / `node:fs` etc. should be treated as leaves (no edge added).
    let fs = MemFs::default().file(
        "/r/a.ts",
        "import react from 'react'\nimport { fs } from 'node:fs'\n",
    );
    let set = collect_reachable(Path::new("/r/a.ts"), Language::Ts, &fs);
    assert_eq!(set.len(), 1, "only the entry file should be reachable");
}

#[test]
fn extra_changed_files_force_affected_status() {
    // git reports nothing, but the caller knows about a config change.
    let fs = MemFs::default()
        .file("/r/svc/a.bench.ts", "// no imports")
        .file("/r/svc/config.ts", "export const X = 1");
    let git = FixedGit {
        root: PathBuf::from("/r"),
        changed: vec![],
        calls: RefCell::new(vec![]),
    };
    let set = resolve_affected(&AffectedOptions {
        cwd: PathBuf::from("/r/svc"),
        base: "origin/master".into(),
        language: Language::Ts,
        patterns: vec![],
        extra_changed_files: vec!["svc/a.bench.ts".into()],
        git: &git,
        fs: &fs,
        import_extractor: None,
    })
    .unwrap();
    assert_eq!(set.bench_files.len(), 1);
    assert!(set.changed_files.iter().any(|f| f.ends_with("a.bench.ts")));
}

#[test]
fn resolve_affected_empty_when_no_diff() {
    let fs = MemFs::default().file("/r/svc/a.bench.ts", "");
    let git = FixedGit {
        root: PathBuf::from("/r"),
        changed: vec![],
        calls: RefCell::new(vec![]),
    };
    let set = resolve_affected(&AffectedOptions {
        cwd: PathBuf::from("/r/svc"),
        base: "origin/master".into(),
        language: Language::Ts,
        patterns: vec![],
        extra_changed_files: vec![],
        git: &git,
        fs: &fs,
        import_extractor: None,
    })
    .unwrap();
    assert_eq!(set.bench_files.len(), 0, "no diff ⇒ no affected benches");
    assert_eq!(
        set.all_bench_files.len(),
        1,
        "discovery still sees the bench"
    );
}

#[test]
fn discovery_excludes_node_modules_and_target() {
    // A bench file under node_modules / target / .git should never be picked
    // up. This is the property that lets aatxe run from a service repo root.
    let fs = MemFs::default()
        .file("/r/svc/real.bench.ts", "")
        .file("/r/svc/node_modules/pkg/fake.bench.ts", "")
        .file("/r/svc/target/debug/build.bench.ts", "")
        .file("/r/svc/.git/hooks/post.bench.ts", "");
    let git = FixedGit {
        root: PathBuf::from("/r"),
        changed: vec!["svc/real.bench.ts".into()],
        calls: RefCell::new(vec![]),
    };
    let set = resolve_affected(&AffectedOptions {
        cwd: PathBuf::from("/r/svc"),
        base: "origin/master".into(),
        language: Language::Ts,
        patterns: vec![],
        extra_changed_files: vec![],
        git: &git,
        fs: &fs,
        import_extractor: None,
    })
    .unwrap();
    assert_eq!(set.all_bench_files.len(), 1, "only real.bench.ts visible");
    assert_eq!(set.bench_files.len(), 1);
}

#[test]
fn rust_path_attribute_redirects_mod_resolution() {
    // `#[path = "alt/d.rs"] mod d;` ⇒ aatxe should reach `alt/d.rs` from the
    // synthetic `alt/d.rs` specifier.
    let fs = MemFs::default()
        .file(
            "/r/lib.rs",
            r#"#[path = "alt/d.rs"]
mod d;
"#,
        )
        .file("/r/alt/d.rs", "pub fn touched() {}");
    let set = collect_reachable(Path::new("/r/lib.rs"), Language::Rust, &fs);
    assert!(
        set.iter().any(|p| p.ends_with("alt/d.rs")),
        "expected alt/d.rs in {:?}",
        set
    );
}

#[test]
fn rust_mod_with_mod_rs_in_subdir_is_resolved() {
    // `mod x;` next to `x/mod.rs` should resolve to `x/mod.rs`.
    let fs = MemFs::default()
        .file("/r/lib.rs", "pub mod x;")
        .file("/r/x/mod.rs", "pub const X: u8 = 1;");
    let set = collect_reachable(Path::new("/r/lib.rs"), Language::Rust, &fs);
    assert!(set.iter().any(|p| p.ends_with("x/mod.rs")));
}

#[test]
fn go_directory_as_package_pulls_every_go_file() {
    // `./shared` in a Go file should reach every .go file under shared/.
    let fs = MemFs::default()
        .file(
            "/r/lib.go",
            r#"package svc
import "./shared"
func _() { _ = shared.X }
"#,
        )
        .file("/r/shared/a.go", "package shared\nconst X = 1")
        .file("/r/shared/b.go", "package shared\nconst Y = 2");
    let set = collect_reachable(Path::new("/r/lib.go"), Language::Go, &fs);
    assert!(set.iter().any(|p| p.ends_with("shared/a.go")));
    assert!(set.iter().any(|p| p.ends_with("shared/b.go")));
}
