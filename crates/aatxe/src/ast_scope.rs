//! CLI-side helper: turn a `HashMap<path, content>` snapshot of the
//! review surface into the rendered "Symbol scope (AST-derived)" string
//! that goes into [`aatxe_council::pipeline::CouncilOptions::ast_scope`].
//!
//! Keeps the pure crate (`aatxe-council`) decoupled from any particular
//! parser — the council takes a plain `String` and this module is the
//! one place that knows about `aatxe-ast`.

use aatxe_ast::{describe, language_for_path, render_scope_block, FileGraph};
use std::collections::HashMap;

/// Build the scope block for a review surface.
///
/// * `files` — every file the council will see, indexed by repo-relative
///   path. In production this is the union of "diff'd file" snapshots and
///   "related context" files.
/// * `changed_paths` — the subset of `files` whose symbols are the focus
///   of this review (the diff's `+++` side).
///
/// Returns an empty string if no file in `files` matches a known
/// language; the council then short-circuits the scope section in the
/// proposer prompt.
pub fn build_scope_for_review(files: &HashMap<String, String>, changed_paths: &[String]) -> String {
    if files.is_empty() || changed_paths.is_empty() {
        return String::new();
    }
    let mut workspace: Vec<(String, FileGraph)> = Vec::with_capacity(files.len());
    for (path, content) in files {
        let Some(lang) = language_for_path(path) else {
            continue;
        };
        let g = describe(lang, path, content);
        workspace.push((path.clone(), g));
    }
    // Deterministic ordering so the rendered block is reproducible.
    workspace.sort_by(|a, b| a.0.cmp(&b.0));
    render_scope_block(&workspace, changed_paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_review_surface_renders_scope() {
        let mut files = HashMap::new();
        files.insert(
            "src/render.rs".to_string(),
            "pub fn render() -> String { String::new() }\n".to_string(),
        );
        files.insert(
            "src/cli.rs".to_string(),
            "use crate::render::render;\nfn main() { render(); }\n".to_string(),
        );
        let scope = build_scope_for_review(&files, &["src/render.rs".to_string()]);
        assert!(
            scope.contains("fn render"),
            "scope should surface the changed fn:\n{scope}"
        );
        assert!(
            scope.contains("[pub]"),
            "scope should mark `render` as exported:\n{scope}"
        );
    }

    #[test]
    fn unknown_extensions_drop_silently() {
        let mut files = HashMap::new();
        files.insert("README.md".to_string(), "# hi\n".to_string());
        files.insert("Cargo.toml".to_string(), "[package]\n".to_string());
        let scope = build_scope_for_review(&files, &["README.md".to_string()]);
        assert!(
            scope.is_empty(),
            "no parseable lang => empty scope, got:\n{scope}"
        );
    }

    #[test]
    fn mixed_language_workspace_renders_only_changed() {
        let mut files = HashMap::new();
        files.insert(
            "main.go".to_string(),
            "package main\nfunc Hello() {}\n".to_string(),
        );
        files.insert(
            "util.ts".to_string(),
            "export function helper(): void {}\n".to_string(),
        );
        files.insert("lib.rs".to_string(), "pub fn build() {}\n".to_string());
        let scope = build_scope_for_review(&files, &["main.go".to_string()]);
        assert!(scope.contains("main.go:"));
        assert!(scope.contains("Hello"));
        assert!(!scope.contains("util.ts"), "non-changed file omitted");
        assert!(!scope.contains("lib.rs"), "non-changed file omitted");
    }

    #[test]
    fn integration_against_real_eval_case_rust_reinvents_counters() {
        // Reproduce what `aatxe evals --council` does for this case so a
        // regression in the diff/path/parsing pipeline shows up here, not
        // only when the full eval is run.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let root = std::path::Path::new(manifest_dir).join("../..");
        let diff_path =
            root.join("evals/council/cases/maintainability-rust-reinvents-counters.diff");
        let files_dir =
            root.join("evals/council/cases/files/maintainability-rust-reinvents-counters");
        if !diff_path.exists() || !files_dir.exists() {
            // Eval corpus not present (e.g. running tests in a stripped
            // workspace). Skip without failing.
            return;
        }
        let diff = std::fs::read_to_string(&diff_path).unwrap();
        let mut files = HashMap::new();
        for entry in walkdir::WalkDir::new(&files_dir) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(&files_dir).unwrap();
            files.insert(
                rel.to_string_lossy().replace('\\', "/"),
                std::fs::read_to_string(entry.path()).unwrap(),
            );
        }
        let changed: Vec<String> = aatxe_council::diff::parse_unified_diff(&diff)
            .into_iter()
            .map(|f| f.path)
            .collect();
        let scope = super::build_scope_for_review(&files, &changed);
        // Expectation: the diff modifies src/handlers/upload.rs which is
        // a Rust file present in the fixture, so the scope must be
        // non-empty. If this fails, the diff-vs-fixture path alignment
        // is broken and the council won't get AST scope on real PRs.
        assert!(!scope.is_empty(), "Rust eval case must produce scope");
        assert!(scope.contains("src/handlers/upload.rs"));
        assert!(
            scope.contains("UploadStats") && scope.contains("upload"),
            "expected struct + handler in scope, got:\n{scope}"
        );
    }

    #[test]
    fn integration_against_real_eval_case_idor_export_route_picks_up_router_arrows() {
        // Lock in the AST-scope coverage for `security-authz-idor-
        // export-route` — the case projects/aatxe.md:204 calls out as
        // 0/3 in the real-LLM run precisely because the describer used
        // to emit zero symbols for its `admin.ts`. The router arrow
        // capture added to the TS describer means the prompt now
        // receives concrete `get /audit-log` and `get /users/:id/export`
        // entries the model can reason about.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let root = std::path::Path::new(manifest_dir).join("../..");
        let diff_path = root.join("evals/council/cases/security-authz-idor-export-route.diff");
        let files_dir = root.join("evals/council/cases/files/security-authz-idor-export-route");
        if !diff_path.exists() || !files_dir.exists() {
            return;
        }
        let diff = std::fs::read_to_string(&diff_path).unwrap();
        let mut files = HashMap::new();
        for entry in walkdir::WalkDir::new(&files_dir) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(&files_dir).unwrap();
            files.insert(
                rel.to_string_lossy().replace('\\', "/"),
                std::fs::read_to_string(entry.path()).unwrap(),
            );
        }
        let changed: Vec<String> = aatxe_council::diff::parse_unified_diff(&diff)
            .into_iter()
            .map(|f| f.path)
            .collect();
        let scope = super::build_scope_for_review(&files, &changed);
        assert!(
            !scope.is_empty(),
            "IDOR eval case must now produce a non-empty AST scope"
        );
        // The route symbols are the load-bearing addition for this case.
        // Path-string is matched verbatim (no slash-stripping).
        assert!(
            scope.contains("/users/:id/export") || scope.contains("get /users/:id/export"),
            "scope must surface the IDOR'd route handler:\n{scope}"
        );
    }

    #[test]
    fn empty_inputs_produce_empty_scope() {
        let scope = build_scope_for_review(&HashMap::new(), &[]);
        assert!(scope.is_empty());
        let mut files = HashMap::new();
        files.insert("x.rs".to_string(), "fn a() {}".to_string());
        let scope = build_scope_for_review(&files, &[]);
        assert!(scope.is_empty(), "no changed paths => empty scope");
    }
}
